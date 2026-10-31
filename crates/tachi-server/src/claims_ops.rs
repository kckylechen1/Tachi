//! Cross-session presence claims (#1001) — daemon-side wiring for
//! `memcore::session_claims`.
//!
//! This module is the one place that composes:
//!   1. session identity (whatever binding exists — HTTP direct-connect
//!      `session_client`, else `TACHI_AGENT_SEAT`, else an anonymous id),
//!   2. the zero-ceremony auto-register/heartbeat hook the briefing/intake/
//!      dispatch call sites use (degrades to no-op on any storage error —
//!      never fails the host action, #1001 Scope item 2),
//!   3. the briefing 工位表 projection + advisory collision warnings, and
//!   4. the canonical Task WorkClaim lifecycle and the legacy presence
//!      release hook used by terminal dispatch paths.
//!
//! Non-goals (frozen in #1001): advisory only, never a mutex/lock; grain is
//! session×issue/lane, never harness-internal subagent granularity.

use crate::shared_defs::CLAIM_TTL_SECONDS;
use std::time::{SystemTime, UNIX_EPOCH};

use icu_properties::{props::GeneralCategory, CodePointMapData};
use memcore::{
    AgentIdentity, ClaimSelector, NewSessionClaim, NewWorkClaim, ReleaseOutcome, SessionClaim,
    UnverifiedAdmissionState, WorkClaimHandoffRequest, WorkClaimMode,
};

use crate::server_state::MemoryServer;

pub(crate) mod project_ownership;
mod run_owner;
pub(crate) use run_owner::{current_run_owner, RunOwner};

pub(crate) fn admit_agent_connection(
    server: &MemoryServer,
    asserted: Option<String>,
    local: bool,
) -> Result<(), String> {
    let connection_id = format!("conn-{}", uuid::Uuid::new_v4());
    let rejection_evidence = match asserted.as_deref() {
        None => Some("agent identity assertion is missing"),
        Some(identity) if crate::session_identity::valid_agent_identity_assertion(identity) => None,
        Some(_) => Some("agent identity assertion is invalid"),
    };
    let identity =
        asserted.filter(|id| crate::session_identity::valid_agent_identity_assertion(id));
    let admission = if identity.is_none() {
        "rejected"
    } else if local {
        "self_asserted"
    } else {
        "unavailable"
    };
    if let Some(rejection_evidence) = rejection_evidence {
        server.with_global_store(|store| {
            memcore::record_rejected_admission(
                store.connection(),
                &format!("admission-{}", uuid::Uuid::new_v4()),
                &connection_id,
                rejection_evidence,
            )
            .map_err(|err| err.to_string())
        })?;
    } else if let Some(identity_id) = identity.as_deref() {
        server.with_global_store(|store| {
            let row = AgentIdentity {
                agent_identity_id: identity_id.to_string(),
                display_name: None,
                seat: None,
                capability_json: None,
                created_at: String::new(),
            };
            match memcore::insert_agent_identity(store.connection(), &row) {
                Ok(()) => {}
                Err(memcore::MemoryError::Sqlite(err))
                    if err.sqlite_error_code()
                        == Some(rusqlite::ErrorCode::ConstraintViolation) => {}
                Err(err) => return Err(err.to_string()),
            }
            memcore::record_unverified_admission(
                store.connection(),
                &format!("admission-{}", uuid::Uuid::new_v4()),
                identity_id,
                &connection_id,
                if local {
                    UnverifiedAdmissionState::SelfAsserted
                } else {
                    UnverifiedAdmissionState::Unavailable
                },
            )
            .map_err(|err| err.to_string())
        })?;
    }
    server.set_work_claim_connection(identity, connection_id, admission.to_string());
    Ok(())
}

/// Public, opaque reference to this connection's existing Host admission.
/// This read grants no authority: attachment still validates the reference,
/// worker grant and WorkClaim at its mutation boundary. Anonymous, unavailable
/// or revoked admissions leave the existing board usable without a reference.
pub(crate) fn current_admission_receipt_ref(
    server: &MemoryServer,
) -> Result<Option<String>, String> {
    // One runtime snapshot binds the identity, connection and verified context.
    let Some(connection) = server.agent_runtime_read().work_claim_connection.clone() else {
        return Ok(None);
    };
    let Some(identity) = connection.agent_identity_id.as_deref() else {
        return Ok(None);
    };
    match connection.admission.as_str() {
        "self_asserted" => server.with_global_store_read(|store| {
            let active: bool = store
                .connection()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM identity_admissions
                 WHERE agent_identity_id=?1 AND connection_id=?2 AND state='self_asserted')",
                    rusqlite::params![identity, connection.connection_id],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            // Local attachments already accept the opaque connection id; do
            // not expose row internals or create a separate receipt.
            Ok(active.then_some(connection.connection_id))
        }),
        "verified" => {
            let Some(context) = connection.verified else {
                return Ok(None);
            };
            if context.agent_identity_id != identity
                || context.connection_id != connection.connection_id
            {
                return Ok(None);
            }
            server.with_global_store_read(|store| {
                let active = memcore::has_current_verified_admission(
                    store.connection(),
                    &context.admission_id,
                    &context.agent_identity_id,
                    &context.connection_id,
                    &context.issuer_id,
                    &context.verification_method,
                    &context.verification_version,
                    &context.trust_domain,
                    &context.verification_scope,
                )
                .map_err(|error| error.to_string())?;
                // Verified attachments require the canonical admission id,
                // never the connection-id alias accepted by local admissions.
                Ok(active.then_some(context.admission_id))
            })
        }
        _ => Ok(None),
    }
}

fn task_required(value: Option<String>, name: &str) -> Result<String, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{name} is required"))
}
fn task_mode(value: Option<String>) -> Result<WorkClaimMode, String> {
    match task_required(value, "claim_mode")?.as_str() {
        "read_only" => Ok(WorkClaimMode::ReadOnly),
        "writable" => Ok(WorkClaimMode::Writable),
        _ => Err("claim_mode must be read_only or writable".to_string()),
    }
}
fn canonical_claim_worktree_path(value: Option<String>) -> Result<String, String> {
    let path = value.unwrap_or_default();
    if path.trim().is_empty() {
        return Ok(path);
    }
    crate::exec_env_ops::canonical_worktree_path(&path)
}
fn task_identity(server: &MemoryServer, asserted: Option<String>) -> Result<String, String> {
    let Some((identity, _connection, admission)) = server.work_claim_connection() else {
        return Err("AgentIdentity admission is unavailable; initialize first".to_string());
    };
    if admission == "rejected" {
        return Err("AgentIdentity admission rejected".to_string());
    }
    if admission == "unavailable" {
        return Err(
            "AgentIdentity admission unavailable; remote identity has no #1170 proof".to_string(),
        );
    }
    let identity = identity.ok_or_else(|| {
        "AgentIdentity admission unavailable; remote identity has no #1170 proof".to_string()
    })?;
    if asserted.is_some_and(|value| value != identity) {
        return Err("agent_identity_id must match the admitted stable identity".to_string());
    }
    Ok(identity)
}

pub(crate) fn handle_task_claim(
    server: &MemoryServer,
    params: &crate::tool_params::TachiTaskParams,
) -> Result<serde_json::Value, String> {
    let scope = serde_json::to_string(&params.claim_scope).map_err(|err| err.to_string())?;
    if params.claim_scope.is_empty() {
        return Err("claim_scope is required".to_string());
    }
    let claim_id = format!("claim-{}", uuid::Uuid::new_v4());
    let mode = task_mode(params.claim_mode.clone())?;
    let claim = NewWorkClaim {
        claim_id: claim_id.clone(),
        agent_identity_id: task_identity(server, params.agent_identity_id.clone())?,
        session_client: server.session_client(),
        issue_ref: params.issue_ref.clone(),
        flow_id: params.flow_id.clone(),
        dispatch_id: params.dispatch_id.clone(),
        branch: params.branch.clone().unwrap_or_default(),
        worktree_path: canonical_claim_worktree_path(params.worktree_path.clone())?,
        declared_file_scope: scope,
        role: task_required(params.claim_role.clone(), "claim_role")?,
        mode,
        expected_head: task_required(params.expected_head.clone(), "expected_head")?,
        lease_expires_at: task_required(params.lease_expires_at.clone(), "lease_expires_at")?,
        created_at: String::new(),
    };
    if let Some(binding) = project_ownership::LocalProject::capture(server) {
        if binding.identity != claim.agent_identity_id {
            return Err(project_ownership::UNAVAILABLE.into());
        }
        if params
            .project
            .as_deref()
            .is_some_and(|p| p.trim() != binding.project)
        {
            return Err(project_ownership::UNAVAILABLE.into());
        }
        binding.with_current(server, |conn| {
            memcore::db::session_claims::insert_local_project_work_claim(
                conn,
                &claim,
                &binding.connection_id,
                &binding.project,
            )
        })?;
    } else {
        crate::verified_admission::with_current_admission_write(server, |conn| {
            memcore::insert_work_claim(conn, &claim)
        })?;
    }
    Ok(
        serde_json::json!({"status":"completed","action":"claim","claim_id":claim_id,"transition_version":0}),
    )
}

pub(crate) fn handle_task_heartbeat(
    server: &MemoryServer,
    params: &crate::tool_params::TachiTaskParams,
) -> Result<serde_json::Value, String> {
    let caller_identity_id = task_identity(server, params.agent_identity_id.clone())?;
    let claim_id = task_required(params.claim_id.clone(), "claim_id")?;
    let transition_version = params
        .transition_version
        .ok_or_else(|| "transition_version is required".to_string())?;
    let lease_expires_at = task_required(params.lease_expires_at.clone(), "lease_expires_at")?;
    let receipt = project_ownership::with_claim_transition(server, &claim_id, |conn| {
        memcore::heartbeat_work_claim(
            conn,
            &claim_id,
            &caller_identity_id,
            transition_version,
            &lease_expires_at,
        )
    })?;
    Ok(
        serde_json::json!({"status":"completed","action":"heartbeat","claim_id":receipt.claim_id,"transition_version":receipt.transition_version,"lease_expires_at":receipt.lease_expires_at}),
    )
}

pub(crate) fn handle_task_handoff(
    server: &MemoryServer,
    params: &crate::tool_params::TachiTaskParams,
) -> Result<serde_json::Value, String> {
    let caller_identity_id = task_identity(server, params.agent_identity_id.clone())?;
    let mode = task_mode(params.claim_mode.clone())?;
    let successor = WorkClaimHandoffRequest {
        agent_identity_id: caller_identity_id.clone(),
        role: task_required(params.claim_role.clone(), "claim_role")?,
        mode,
        worktree_path: canonical_claim_worktree_path(params.worktree_path.clone())?,
        declared_file_scope: serde_json::to_string(&params.claim_scope)
            .map_err(|err| err.to_string())?,
        expected_head: task_required(params.expected_head.clone(), "expected_head")?,
        lease_expires_at: task_required(params.lease_expires_at.clone(), "lease_expires_at")?,
    };
    let claim_id = task_required(params.claim_id.clone(), "claim_id")?;
    let transition_version = params
        .transition_version
        .ok_or_else(|| "transition_version is required".to_string())?;
    let receipt = project_ownership::with_claim_transition_checked(
        server,
        &claim_id,
        || memcore::db::session_claims::validate_work_claim_handoff(&successor),
        |conn| {
            memcore::handoff_work_claim(
                conn,
                &claim_id,
                &caller_identity_id,
                transition_version,
                &successor,
            )
        },
    )?;
    Ok(
        serde_json::json!({"status":"completed","action":"handoff","claim_id":receipt.claim_id,"transition_version":receipt.transition_version,"to_agent_identity_id":receipt.to_agent_identity_id}),
    )
}

pub(crate) fn handle_task_release(
    server: &MemoryServer,
    params: &crate::tool_params::TachiTaskParams,
) -> Result<serde_json::Value, String> {
    let caller_identity_id = task_identity(server, params.agent_identity_id.clone())?;
    let claim_id = task_required(params.claim_id.clone(), "claim_id")?;
    let transition_version = params
        .transition_version
        .ok_or_else(|| "transition_version is required".to_string())?;
    let release_reason = params
        .release_reason
        .as_deref()
        .unwrap_or("explicit_release");
    let version = project_ownership::with_claim_transition(server, &claim_id, |conn| {
        memcore::release_work_claim(
            conn,
            &claim_id,
            &caller_identity_id,
            transition_version,
            release_reason,
        )
    })?;
    Ok(
        serde_json::json!({"status":"completed","action":"release","claim_id":claim_id,"transition_version":version}),
    )
}

pub(crate) fn work_claim_board(server: &MemoryServer) -> Result<serde_json::Value, String> {
    let claims = server.with_global_store_read(|store| {
        memcore::db::session_claims::list_unbound_claims(store.connection(), None)
            .map_err(|err| err.to_string())
    })?;
    Ok(
        serde_json::json!({"github_state":"unavailable","work_claims":claims.into_iter().map(|claim| serde_json::json!({"claim_id":claim.claim_id,"issue_ref":claim.issue_ref,"state":claim.state.as_str(),"transition_version":claim.transition_version,"agent_identity_id":claim.agent_identity_id})).collect::<Vec<_>>() }),
    )
}

/// Max length (chars) for a sanitized identifier-shaped presence field
/// (`session_client`, `issue_ref`, `flow_id`, `branch`, `heartbeat_at`).
const SANITIZE_IDENTIFIER_CAP: usize = 64;
/// Max length (chars) for a sanitized free-text-shaped presence field
/// (a collision warning line, a declared file-scope path).
const SANITIZE_TEXT_CAP: usize = 160;

/// #1001 round 2 item 5 / round 3 item 5 / round 4 (codex "markdown
/// injection" finding, now hardened a 2nd time after round 3 was found still
/// incomplete): every presence field originates from ANOTHER session/agent's
/// caller-supplied strings (`session_client`, `issue_ref`, `flow_id`,
/// `branch`, `declared_file_scope`) and is interpolated verbatim into
/// another session's briefing markdown
/// (`agent_markdown::briefing::format_briefing`,
/// `copilot_ops::feature_briefing::markdown::markdown_presence_section`)
/// with no escaping or bounds. A newline, Markdown-metacharacter, trailing
/// backslash, or Unicode bidi/format payload in one of those fields could
/// alter the rendered structure of a DIFFERENT session's briefing — close
/// **bold**, open a link/HTML-shaped span, escape the fixed closing `**` a
/// renderer writes, or visually reorder text via a bidi override (the
/// injection vector the mission calls out). This is the single sanitize
/// choke point both the board projection (`briefing_claims_board`) and the
/// collision-warning strings (`collision_warnings`) route every
/// caller-supplied field through before it is placed in a
/// `serde_json::Value`/`String` that a renderer will later interpolate — so
/// both consumers (feature-briefing markdown and the legacy
/// `format_briefing` markdown) inherit the same sanitization from one
/// source, rather than each renderer having to remember to escape on read.
///
/// Sanitization, in order:
/// 1. Strip ASCII control characters (including `\n`/`\r`, which is what lets
///    a claim's field break out of its single markdown list-item line) and
///    Unicode `Cc` control characters.
/// 2. Strip every Unicode `Cf` (format) character, by CATEGORY rather than by
///    a hand-maintained enumeration — round 3's fix used an explicit code
///    point list (`is_bidi_or_format_char`, since removed) that missed
///    `Cf` characters outside the specific bidi/ZW set it enumerated (e.g.
///    U+061C ARABIC LETTER MARK, round-4 codex finding); this is the second
///    time a hand-maintained enumeration under-covered a category, so the
///    fix is now a real category lookup
///    (`icu_properties::CodePointMapData::<GeneralCategory>::new()`, the
///    `unicode-general-category` role filled by a crate already resolved
///    in this workspace's graph — see the `icu_properties` dependency
///    comment in `Cargo.toml`) that covers every `Cf` code point by
///    construction, current and future Unicode versions alike, not just the
///    ones an author happened to enumerate. `char::is_control()` only
///    covers `Cc`, not `Cf` — a bidi override or format character is fully
///    "printable" by that definition, so it survives step 1 untouched and
///    can still reorder how the rendered line visually reads, or hide
///    payload, even though the underlying bytes are unchanged
///    (`is_unicode_format_char`).
/// 3. **Escape, not delete**, Markdown-active metacharacters INCLUDING
///    backslash itself (`\ * _ \` [ ] ( ) # < > | ~`) by prefixing each with
///    a literal `\` (the same CommonMark backslash-escape a human author
///    would type to neutralize a metacharacter while keeping it legible) —
///    round 4 replaced these with a bare space, which destroyed real values
///    a claim legitimately carries (`claims_ops.rs`'s own filename, a
///    `flow_id` like `owner/repo#1042`) instead of just neutralizing the
///    Markdown activation (#1023: round 4 fixed the injection but broke
///    fidelity by deleting instead of escaping). Escaping still guarantees a
///    claim field can never close/open emphasis, links, headings, inline
///    HTML/autolinks, table cells, or strikethrough in a DIFFERENT session's
///    rendered briefing, and can never escape the renderer's own fixed
///    closing `**` either — every consumer of this field renders it raw
///    (`format!("- **{session}** → {target} ...")`), so a `session` value
///    ending in a bare `\` would otherwise make the literal text end in
///    `\**`, which a CommonMark-compliant renderer reads as an escaped
///    literal `*` followed by one still-open `*`, leaving emphasis open past
///    the intended closing marker (round-4 codex finding). Backslash is
///    escaped in the SAME left-to-right pass as every other metacharacter
///    (each input char is inspected exactly once, never the characters this
///    function itself just emitted) so a literal backslash in the input can
///    never be mistaken for an escape marker this function produced, and
///    never gets escaped twice. The guarantee must live here, not be an
///    opt-in the renderer remembers to apply.
/// 4. Collapse to a single line (whitespace-joined), trim, then cap to `cap`
///    chars with a `…` suffix when truncated. The cap is enforced on the
///    ESCAPED string, so truncation can land in the middle of a two-char
///    escape pair (`\` + metachar); when it does, the trailing bare `\` is
///    dropped rather than kept as a dangling escape marker with nothing
///    after it (which would itself misparse in a renderer, e.g. escaping
///    into the `…` suffix or a subsequent line).
pub(crate) fn sanitize_presence_field(raw: &str, cap: usize) -> String {
    const MD_METACHARS: &[char] = &['*', '_', '`', '[', ']', '(', ')', '#', '<', '>', '|', '~'];
    let mut stripped = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if is_unicode_format_char(ch) {
            continue;
        }
        if ch.is_control() {
            stripped.push(' ');
        } else if ch == '\\' || MD_METACHARS.contains(&ch) {
            // Same pass, same input char: escape backslash itself here too,
            // so this loop never re-scans (and therefore never re-escapes)
            // a `\` it just emitted for some other metacharacter.
            stripped.push('\\');
            stripped.push(ch);
        } else {
            stripped.push(ch);
        }
    }
    let one_line = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
    let one_line = one_line.trim();
    if one_line.is_empty() {
        return String::new();
    }
    cap_already_sanitized(one_line, cap)
}

/// Bound an ALREADY-escaped/collapsed field to `cap` chars with a `…`
/// suffix, applying the same truncation-boundary guard as
/// [`sanitize_presence_field`] (never split a `\` + metachar escape pair,
/// leaving a dangling `\`) — WITHOUT re-running the strip/escape passes.
///
/// [`collision_warnings`] composes several pieces that are EACH already
/// individually run through [`sanitize_presence_field`] (`claim_session`,
/// `claim_heartbeat`, `safe_issue_ref`/`safe_overlap`) into one warning
/// line, and only needs to re-bound the TOTAL composed length (a
/// multi-path `safe_overlap` join can exceed `SANITIZE_TEXT_CAP` even
/// though each individual path was already capped at
/// `SANITIZE_IDENTIFIER_CAP`). Re-running full `sanitize_presence_field` on
/// that ALREADY-escaped composed string would be a real bug under the
/// escape-not-delete scheme: escaping is NOT idempotent under
/// re-application the way deletion was (round 3/4's delete-based sanitizer
/// happened to tolerate double-application harmlessly — deleting an
/// already-deleted metachar is a no-op; escaping an already-escaped `\_`
/// pair a second time corrupts it into `\\\_`). This function is the
/// length-only half of sanitization, safe to apply to already-sanitized
/// input any number of times.
fn cap_already_sanitized(s: &str, cap: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= cap {
        return s.to_string();
    }
    let keep = cap.saturating_sub(1).max(1);
    let mut kept: Vec<char> = s.chars().take(keep).collect();
    // Truncation boundary guard: never leave a lone trailing `\` that was
    // meant to escape the character truncation just cut away — that bare
    // backslash would itself be live Markdown-escape syntax against
    // whatever follows it (the `…` suffix), the opposite of "safe".
    if kept.last() == Some(&'\\') {
        kept.pop();
    }
    format!("{}…", kept.into_iter().collect::<String>())
}

/// True iff `ch`'s Unicode `General_Category` is `Cf` (Format) — bidi
/// controls, zero-width joiners/non-joiners, the BOM, soft hyphen, the
/// Arabic Letter Mark (U+061C), and every other code point Unicode assigns
/// to the Format category, current and future versions alike. Backed by
/// `icu_properties`' compiled Unicode Character Database data
/// (`compiled_data` feature, on by default — no network fetch, no runtime
/// data file), not a hand-maintained enumeration: round 3 of this fix
/// enumerated a specific bidi/zero-width code point set
/// (`is_bidi_or_format_char`) that covered the vector's most common
/// instances but under-covered the category itself (missed U+061C and any
/// other `Cf` code point outside that list) — the exact "manual subset,
/// not all `Cf`" gap codex's round-4 review called out. A real category
/// classifier closes that gap by construction instead of by a second round
/// of manual additions.
fn is_unicode_format_char(ch: char) -> bool {
    CodePointMapData::<GeneralCategory>::new().get(ch) == GeneralCategory::Format
}

/// [`sanitize_presence_field`] scoped to an `Option<&str>` identifier-shaped
/// field, with the identifier cap. Returns `None` unchanged (a missing field
/// stays missing) and `Some(String::new())` collapses to `None` (an
/// all-control-chars/whitespace input sanitizes away to nothing, which
/// should render the same as "field absent", not an empty bold/backtick
/// span).
fn sanitize_presence_identifier(raw: Option<&str>) -> Option<String> {
    raw.map(|s| sanitize_presence_field(s, SANITIZE_IDENTIFIER_CAP))
        .filter(|s| !s.is_empty())
}

/// Resolve the caller's session identity for a claim, in the mission's stated
/// precedence: whatever session binding exists (HTTP direct-connect
/// `session_client`) → `TACHI_AGENT_SEAT` env var (read-only; a sibling PR
/// owns setting it, this module only reads) → an anonymous session id.
///
/// The anonymous fallback is stable for the lifetime of one process (derived
/// from pid), not a fresh id per call, so heartbeats from the same anonymous
/// process still coalesce onto one claim row via `upsert_or_heartbeat_claim`'s
/// identity key.
pub(crate) fn resolve_session_client(server: &MemoryServer) -> String {
    if let Some(bound) = server.session_client().filter(|s| !s.trim().is_empty()) {
        return bound;
    }
    if let Ok(seat) = std::env::var("TACHI_AGENT_SEAT") {
        let trimmed = seat.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    format!("anon-{}", std::process::id())
}

/// Generate a unique claim id candidate. Timestamp-nanos XOR pid keeps it
/// collision-free across concurrent claims on one host, same recipe as
/// `exec_env_ops::generate_env_id`. Note: `upsert_or_heartbeat_claim` may
/// discard this candidate in favor of an existing row's id when one already
/// matches the (session_client, issue_ref, flow_id) identity — the candidate
/// is only actually persisted on a fresh insert.
fn generate_claim_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mixed = nanos ^ ((std::process::id() as u128) << 64);
    format!("claim-{mixed:032x}")
}

/// Fields a hook call site has on hand to auto-register/heartbeat a claim.
/// All fields besides `session_client` are optional — a hook may only know
/// `issue_ref` (intake), or `dispatch_id`+`branch` (dispatch), etc.
#[derive(Debug, Clone, Default)]
pub(crate) struct ClaimHookInput {
    pub issue_ref: Option<String>,
    pub flow_id: Option<String>,
    pub dispatch_id: Option<String>,
    pub branch: Option<String>,
    pub declared_file_scope: Option<Vec<String>>,
}

/// THE zero-ceremony hook (#1001 Scope item 2): auto-register-or-heartbeat a
/// claim from inside briefing/intake/dispatch, without the caller having to
/// invoke a separate tool. Degrades to no-op on ANY error — a poisoned claims
/// table, a lock timeout, a missing table on an old DB, must never fail the
/// host action (briefing/intake/dispatch) that called this. Errors are
/// swallowed after a `tracing::warn!`, matching the non-fatal
/// lease-insert-failure precedent in `exec_env_ops::provision_managed_env`.
///
/// A no-op when both `issue_ref` and `flow_id` are absent or blank — there is
/// nothing identifiable to claim (matches nothing in the briefing/collision
/// surfaces either). The values themselves remain verbatim for real claims;
/// trimming here only decides whether there is an identity at all.
pub(crate) fn auto_register_or_heartbeat_claim(server: &MemoryServer, input: &ClaimHookInput) {
    let has_identity = [input.issue_ref.as_deref(), input.flow_id.as_deref()]
        .into_iter()
        .flatten()
        .any(|value| !value.trim().is_empty());
    if !has_identity {
        return;
    }
    let session_client = resolve_session_client(server);
    let new_claim = NewSessionClaim {
        claim_id: generate_claim_id(),
        session_client: Some(session_client),
        issue_ref: input.issue_ref.clone(),
        flow_id: input.flow_id.clone(),
        dispatch_id: input.dispatch_id.clone(),
        branch: input.branch.clone().unwrap_or_default(),
        declared_file_scope: input
            .declared_file_scope
            .as_ref()
            .map(|scope| serde_json::to_string(scope).unwrap_or_default()),
        created_at: String::new(),
    };
    let result: Result<String, String> = server.with_global_store(|store| {
        memcore::upsert_or_heartbeat_claim(store.connection_mut(), &new_claim)
            .map_err(|e| e.to_string())
    });
    if let Err(err) = result {
        tracing::warn!(
            "presence claim auto-register/heartbeat degraded to no-op (#1001 fail-safe): {err}"
        );
    }
}

/// Fail-safe release-by-dispatch-id (#1001 round 2, item 1): the `complete`
/// and `cancel` call sites that registered a claim via
/// `auto_register_or_heartbeat_claim` (keyed on `dispatch_id`) must release it
/// through THE single release path (`memcore::release_claim`) when the
/// dispatch reaches a terminal state — otherwise the row is left `active`
/// forever and the briefing 工位表 keeps showing a session that is gone.
///
/// Same non-fatal discipline as the auto-register hook: a storage error here
/// must never fail `tachi_complete` or
/// `tachi_staff(action='cancel', dispatch_id, expected_status_revision)`, so
/// this degrades to a `tracing::warn!` no-op rather than propagating. A no-op
/// when `dispatch_id` is empty — nothing to release.
pub(crate) fn release_claim_for_dispatch(server: &MemoryServer, dispatch_id: &str, reason: &str) {
    if dispatch_id.trim().is_empty() {
        return;
    }
    let selector = ClaimSelector::DispatchId(dispatch_id.to_string());
    let result: Result<ReleaseOutcome, String> = server.with_global_store(|store| {
        memcore::release_claim(store.connection_mut(), &selector, Some(reason))
            .map_err(|e| e.to_string())
    });
    match result {
        Ok(ReleaseOutcome::Released { claim_id }) => {
            tracing::debug!(
                "presence claim {claim_id} released for dispatch_id={dispatch_id} (reason={reason})"
            );
        }
        Ok(ReleaseOutcome::AlreadyReleased { .. }) | Ok(ReleaseOutcome::NotFound) => {
            // No live claim to release — not an error (e.g. the auto-register
            // hook degraded to no-op earlier, or presence was never claimed
            // for this dispatch).
        }
        Err(err) => {
            tracing::warn!(
                "presence claim release degraded to no-op for dispatch_id={dispatch_id} (#1001 fail-safe): {err}"
            );
        }
    }
}

/// Read-side projection for the briefing 工位表: all claims presently
/// considered active (lazy TTL expiry applied), newest heartbeat first.
/// Read failures degrade to an empty list — briefing must never fail because
/// the presence layer is unavailable.
pub(crate) fn list_live_claims_for_briefing(server: &MemoryServer) -> Vec<SessionClaim> {
    let now = chrono::Utc::now();
    server
        .with_global_store_read(|store| -> Result<Vec<SessionClaim>, String> {
            memcore::db::session_claims::list_unbound_claims(store.connection(), None)
                .map(|claims| {
                    claims
                        .into_iter()
                        .filter(|claim| {
                            claim.state == memcore::ClaimState::Active
                                && !memcore::is_claim_stale(claim, now, CLAIM_TTL_SECONDS)
                        })
                        .collect()
                })
                .map_err(|e| e.to_string())
        })
        .unwrap_or_default()
}

/// Resolve `session_client`'s own live `declared_file_scope`, for forwarding
/// into `collision_warnings` as `new_scope` from a read-only briefing call
/// site that has no scope of its own on hand (#1001 round 2 item 3). Prefers
/// a live claim that also matches `issue_ref` (the more specific identity a
/// caller scoped its briefing call to); falls back to any live claim for
/// `session_client` when `issue_ref` is absent or doesn't match one. Returns
/// an empty `Vec` (board-only, no scope-overlap warnings) when the session
/// has no live claim or its claim carries no declared scope — never panics,
/// never errors.
fn own_live_claim_scope(
    live_claims: &[SessionClaim],
    session_client: &str,
    agent_identity_id: Option<&str>,
    issue_ref: Option<&str>,
) -> Vec<String> {
    let mine = |c: &&SessionClaim| {
        c.session_client.as_deref() == Some(session_client)
            || agent_identity_id.is_some() && c.agent_identity_id.as_deref() == agent_identity_id
    };
    let claim = issue_ref
        .and_then(|want| {
            live_claims
                .iter()
                .find(|c| mine(c) && c.issue_ref.as_deref() == Some(want))
        })
        .or_else(|| live_claims.iter().find(mine));
    claim
        .and_then(|c| c.declared_file_scope.as_deref())
        .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        .unwrap_or_default()
}

/// Advisory collision check (#1001 Scope item 4): does an *other* live claim
/// already sit on the same `issue_ref`, or does its `declared_file_scope`
/// overlap `new_scope`? Never blocks — the caller surfaces this as a warning
/// line, not an error. `exclude_session_client` lets a caller ignore its own
/// prior claim (re-claiming the same issue from the same session is not a
/// collision).
#[cfg(test)]
pub(crate) fn collision_warnings(
    live_claims: &[SessionClaim],
    exclude_session_client: Option<&str>,
    issue_ref: Option<&str>,
    new_scope: &[String],
) -> Vec<String> {
    collision_warnings_excluding(
        live_claims,
        exclude_session_client,
        None,
        issue_ref,
        new_scope,
    )
}

fn collision_warnings_excluding(
    live_claims: &[SessionClaim],
    exclude_session_client: Option<&str>,
    exclude_agent_identity_id: Option<&str>,
    issue_ref: Option<&str>,
    new_scope: &[String],
) -> Vec<String> {
    let mut warnings = Vec::new();
    for claim in live_claims {
        if claim_matches_exclusion(claim, exclude_session_client, exclude_agent_identity_id) {
            continue;
        }
        // #1001 round 2 item 5: every value below (session_client, the
        // scope-overlap path strings) is caller-supplied data from ANOTHER
        // session's claim, sanitized here before it is folded into a warning
        // string that gets interpolated verbatim into a DIFFERENT session's
        // briefing markdown.
        let claim_session = sanitize_presence_field(
            claim.session_client.as_deref().unwrap_or("unknown"),
            SANITIZE_IDENTIFIER_CAP,
        );
        let claim_heartbeat = sanitize_presence_field(&claim.heartbeat_at, SANITIZE_IDENTIFIER_CAP);
        if let (Some(issue_ref), Some(claim_issue)) = (issue_ref, claim.issue_ref.as_deref()) {
            if issue_ref == claim_issue {
                let safe_issue_ref = sanitize_presence_field(issue_ref, SANITIZE_IDENTIFIER_CAP);
                // #1023: cap-only, NOT a second `sanitize_presence_field`
                // pass — every interpolated piece above is already
                // individually escaped; re-escaping the composed string
                // would double-escape it (see `cap_already_sanitized` doc).
                warnings.push(cap_already_sanitized(
                    &format!(
                        "double-claim: {safe_issue_ref} already has a live claim from {claim_session} (heartbeat {claim_heartbeat})"
                    ),
                    SANITIZE_TEXT_CAP,
                ));
            }
        }
        if !new_scope.is_empty() {
            if let Some(existing_scope) = claim
                .declared_file_scope
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
            {
                let overlap: Vec<&String> = new_scope
                    .iter()
                    .filter(|path: &&String| existing_scope.contains(path))
                    .collect();
                if !overlap.is_empty() {
                    let safe_overlap = overlap
                        .iter()
                        .map(|s| sanitize_presence_field(s, SANITIZE_IDENTIFIER_CAP))
                        .collect::<Vec<_>>()
                        .join(", ");
                    // #1023: cap-only (see above) — `claim_session`,
                    // `claim_heartbeat`, and every path in `safe_overlap`
                    // are already individually escaped.
                    warnings.push(cap_already_sanitized(
                        &format!(
                            "file-scope overlap with live claim from {claim_session} (heartbeat {claim_heartbeat}): {safe_overlap}"
                        ),
                        SANITIZE_TEXT_CAP,
                    ));
                }
            }
        }
    }
    warnings
}

fn claim_matches_exclusion(
    claim: &SessionClaim,
    exclude_session_client: Option<&str>,
    exclude_agent_identity_id: Option<&str>,
) -> bool {
    exclude_session_client.is_some() && claim.session_client.as_deref() == exclude_session_client
        || exclude_agent_identity_id.is_some()
            && claim.agent_identity_id.as_deref() == exclude_agent_identity_id
}

/// Max rows [`briefing_claims_board`] puts in `items` — aligned with #1004's
/// top-N + overflow-marker convention (JSON side carries the total/overflow
/// counts; a markdown renderer shows the capped rows and a "+N more" note).
/// #527 CONCERN: an unbounded presence board — every live claim, no cap —
/// grows every briefing payload linearly with fleet size; a handful of the
/// most relevant (freshest) claims is what a briefing reader actually needs.
const PRESENCE_BOARD_DISPLAY_CAP: usize = 5;

/// Briefing 工位表 projection: compact JSON rows the markdown/JSON briefing
/// surfaces both render. Read-failure-safe (empty on any storage error).
///
/// Every field is routed through [`sanitize_presence_field`] (#1001 round 2
/// item 5) before landing in the JSON row — this is caller-supplied data
/// from POTENTIALLY ANOTHER session, rendered verbatim into markdown by both
/// briefing surfaces, so it must never carry newlines/control chars/
/// unbounded length into a DIFFERENT session's briefing output.
///
/// `items` is capped at [`PRESENCE_BOARD_DISPLAY_CAP`] (#527); `count` is
/// the TOTAL live-claim count (not just what's shown) and `overflow` is how
/// many live claims were cut off (`0` when everything fit) — both markdown
/// renderers (`agent_markdown::briefing::format_briefing`,
/// `copilot_ops::feature_briefing::markdown::markdown_presence_section`)
/// read `overflow` to append a "+N more" note rather than silently dropping
/// rows with no indication anything was cut.
pub(crate) fn briefing_claims_board(server: &MemoryServer) -> serde_json::Value {
    let live = list_live_claims_for_briefing(server);
    let total = live.len();
    let rows: Vec<serde_json::Value> = live
        .iter()
        .take(PRESENCE_BOARD_DISPLAY_CAP)
        .map(sanitize_board_row)
        .collect();
    let overflow = total.saturating_sub(rows.len());
    serde_json::json!({
        "count": total,
        "items": rows,
        "overflow": overflow,
    })
}

/// The single sanitized-row shape both the briefing 工位表
/// ([`briefing_claims_board`]) and the #1016 peer-publication read surface
/// ([`project_peer_presence`]) emit — every caller-supplied field routed
/// through the same [`sanitize_presence_field`]/[`sanitize_presence_identifier`]
/// choke point (#1001 round 2 item 5), so a claim from ANOTHER session can
/// never carry newlines/control chars/active-markdown/unbounded length into a
/// reader's rendered output regardless of which surface renders it.
fn sanitize_board_row(c: &SessionClaim) -> serde_json::Value {
    serde_json::json!({
        "session_client": sanitize_presence_identifier(display_session_client(c)),
        "issue_ref": sanitize_presence_identifier(c.issue_ref.as_deref()),
        "flow_id": sanitize_presence_identifier(c.flow_id.as_deref()),
        "branch": sanitize_presence_field(&c.branch, SANITIZE_IDENTIFIER_CAP),
        "heartbeat_at": sanitize_presence_field(&c.heartbeat_at, SANITIZE_IDENTIFIER_CAP),
    })
}

fn display_session_client(c: &SessionClaim) -> Option<&str> {
    match c.session_client.as_deref() {
        Some(session_client) if is_work_claim_compat_session_client(session_client) => {
            c.agent_identity_id.as_deref()
        }
        other => other,
    }
}

fn is_work_claim_compat_session_client(value: &str) -> bool {
    value
        .strip_prefix("work-claim:")
        .is_some_and(|id| !id.trim().is_empty())
}

/// Result of projecting a set of snapshot-time-fresh presence claims into the
/// #1016 peer-publication `result` shape.
pub(crate) struct PeerPresenceProjection {
    /// `{count, items, overflow}` — `count` is the ALIVE-at-render total,
    /// `items` the top-[`PRESENCE_BOARD_DISPLAY_CAP`] freshest alive rows,
    /// `overflow` the alive rows beyond the cap.
    pub board: serde_json::Value,
    /// Rows that were fresh when the read-only snapshot was taken but crossed
    /// the TTL horizon before this projection ran (#1016 sol invariant 7) —
    /// surfaced flagged, NEVER folded into `count`/`items` where they would
    /// masquerade as live. Capped at [`PRESENCE_BOARD_DISPLAY_CAP`].
    pub expired_during_render: Vec<serde_json::Value>,
    /// Freshest alive `heartbeat_at` (raw server-stamped timestamp), or `None`
    /// when there is no alive row. This is the source `as_of`.
    pub as_of: Option<String>,
}

/// Project snapshot-time-fresh presence `candidates` into the peer-publication
/// `result` shape, re-applying the lazy TTL a SECOND time at render/serialize
/// time (#1016 sol invariant 7).
///
/// `candidates` are the rows [`memcore::list_active_claims`] already filtered
/// with the SNAPSHOT clock (so all were fresh when the read-only transaction
/// ran). Between that read and this projection the render clock (`now_render`)
/// can advance past a row's TTL horizon; such a row is emitted under
/// `expired_during_render` and excluded from the alive `count`/`items` — an
/// expired heartbeat must never be presented as a live seat. Reuses the shared
/// [`sanitize_board_row`] shaping and [`PRESENCE_BOARD_DISPLAY_CAP`] so the
/// peer read surface and the briefing 工位表 render identical, equally-safe
/// rows.
pub(crate) fn project_peer_presence(
    candidates: &[SessionClaim],
    now_render: chrono::DateTime<chrono::Utc>,
    ttl_seconds: i64,
) -> PeerPresenceProjection {
    let mut alive: Vec<&SessionClaim> = Vec::new();
    let mut expired: Vec<&SessionClaim> = Vec::new();
    for claim in candidates {
        if memcore::is_claim_stale(claim, now_render, ttl_seconds) {
            expired.push(claim);
        } else {
            alive.push(claim);
        }
    }

    let alive_total = alive.len();
    let items: Vec<serde_json::Value> = alive
        .iter()
        .copied()
        .take(PRESENCE_BOARD_DISPLAY_CAP)
        .map(sanitize_board_row)
        .collect();
    let overflow = alive_total.saturating_sub(items.len());

    // `candidates` arrive newest-heartbeat-first (`list_claims` ORDER BY
    // heartbeat_at DESC), and the partition above preserves that order, so the
    // first alive row is the freshest.
    let as_of = alive
        .first()
        .map(|c| c.heartbeat_at.clone())
        .filter(|hb| !hb.is_empty());

    let expired_during_render: Vec<serde_json::Value> = expired
        .iter()
        .copied()
        .take(PRESENCE_BOARD_DISPLAY_CAP)
        .map(|c| {
            let mut row = sanitize_board_row(c);
            if let Some(obj) = row.as_object_mut() {
                obj.insert(
                    "expired_during_render".to_string(),
                    serde_json::Value::Bool(true),
                );
            }
            row
        })
        .collect();

    PeerPresenceProjection {
        board: serde_json::json!({
            "count": alive_total,
            "items": items,
            "overflow": overflow,
        }),
        expired_during_render,
        as_of,
    }
}

/// THE single entry point (#1001 Scope item 3) both briefing surfaces
/// (`tachi_memory(action='briefing')` and `tachi_task` feature briefing) call
/// to get the presence 工位表 section: one read of live claims, the board
/// projection, and advisory collision warnings scoped to `issue_ref` (if the
/// caller has one). Consolidating this here — rather than each briefing
/// surface re-deriving board+warnings from `list_live_claims_for_briefing`
/// inline — keeps future briefing-assembly changes (#964, #1000) from having
/// to touch presence wiring in two places to stay in sync.
///
/// ## File-scope collision (#1001 round 2 item 3)
///
/// The original slice always passed `&[]` as `new_scope`, which meant the
/// file-scope-overlap half of `collision_warnings` was structurally
/// unreachable from briefing — `collision_warnings` only emits a
/// file-scope-overlap warning when `new_scope` is non-empty. This resolves
/// the calling session's OWN live claim (by `resolve_session_client`,
/// preferring one that also matches `issue_ref` when given, else any live
/// claim for that session) and forwards its `declared_file_scope` as
/// `new_scope`, so a second session whose scope overlaps what THIS session
/// already declared actually surfaces a warning. `exclude_session_client` is
/// set to the resolved session so the session's own claim is never reported
/// as colliding with itself. Falls back to board-only (empty scope, no
/// self-exclusion change in behavior) when this session has no live claim to
/// read a scope from — never fails or panics.
///
/// Read-failure-safe: every step degrades to empty on storage error, so this
/// never fails the briefing call that invokes it.
pub(crate) fn presence_briefing_section(
    server: &MemoryServer,
    issue_ref: Option<&str>,
) -> serde_json::Value {
    let live = list_live_claims_for_briefing(server);
    let board = briefing_claims_board(server);
    let session_client = resolve_session_client(server);
    let agent_identity_id = server
        .work_claim_connection()
        .and_then(|(identity, _, admission)| {
            if admission == "rejected" || admission == "unavailable" {
                None
            } else {
                identity
            }
        });
    let own_scope = own_live_claim_scope(
        &live,
        &session_client,
        agent_identity_id.as_deref(),
        issue_ref,
    );
    let warnings = collision_warnings_excluding(
        &live,
        Some(session_client.as_str()),
        agent_identity_id.as_deref(),
        issue_ref,
        &own_scope,
    );
    serde_json::json!({
        "board": board,
        "warnings": warnings,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "exec_env/tests/canonical_paths.rs"]
mod canonical_path_tests;

#[cfg(test)]
#[path = "claims_ops/ownership_transition_tests.rs"]
mod ownership_transition_tests;

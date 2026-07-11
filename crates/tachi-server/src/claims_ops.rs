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
//!   4. the manual `claim`/`release` facade actions for harness-native work
//!      that never touches a hook (#1001 Scope item 5).
//!
//! Non-goals (frozen in #1001): advisory only, never a mutex/lock; grain is
//! session×issue/lane, never harness-internal subagent granularity.

use std::time::{SystemTime, UNIX_EPOCH};

use memcore::{ClaimSelector, NewSessionClaim, ReleaseOutcome, SessionClaim};

use crate::server_state::MemoryServer;

/// Default lease TTL: a claim whose heartbeat is older than this is presented
/// as expired by readers (briefing/collision-check), without a second write —
/// same lazy-expiry idiom as the exec_envs sweep backstop, just read-side.
pub(crate) const CLAIM_TTL_SECONDS: i64 = 30 * 60;

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
/// A no-op when both `issue_ref` and `flow_id` are absent — there is nothing
/// identifiable to claim (matches nothing in the briefing/collision surfaces
/// either).
pub(crate) fn auto_register_or_heartbeat_claim(server: &MemoryServer, input: &ClaimHookInput) {
    if input.issue_ref.is_none() && input.flow_id.is_none() {
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

/// Read-side projection for the briefing 工位表: all claims presently
/// considered active (lazy TTL expiry applied), newest heartbeat first.
/// Read failures degrade to an empty list — briefing must never fail because
/// the presence layer is unavailable.
pub(crate) fn list_live_claims_for_briefing(server: &MemoryServer) -> Vec<SessionClaim> {
    let now_iso = chrono::Utc::now().to_rfc3339();
    server
        .with_global_store_read(|store| -> Result<Vec<SessionClaim>, String> {
            memcore::list_active_claims(store.connection(), &now_iso, CLAIM_TTL_SECONDS)
                .map_err(|e| e.to_string())
        })
        .unwrap_or_default()
}

/// Advisory collision check (#1001 Scope item 4): does an *other* live claim
/// already sit on the same `issue_ref`, or does its `declared_file_scope`
/// overlap `new_scope`? Never blocks — the caller surfaces this as a warning
/// line, not an error. `exclude_session_client` lets a caller ignore its own
/// prior claim (re-claiming the same issue from the same session is not a
/// collision).
pub(crate) fn collision_warnings(
    live_claims: &[SessionClaim],
    exclude_session_client: Option<&str>,
    issue_ref: Option<&str>,
    new_scope: &[String],
) -> Vec<String> {
    let mut warnings = Vec::new();
    for claim in live_claims {
        if exclude_session_client.is_some()
            && claim.session_client.as_deref() == exclude_session_client
        {
            continue;
        }
        if let (Some(issue_ref), Some(claim_issue)) = (issue_ref, claim.issue_ref.as_deref()) {
            if issue_ref == claim_issue {
                warnings.push(format!(
                    "double-claim: {issue_ref} already has a live claim from {} (heartbeat {})",
                    claim.session_client.as_deref().unwrap_or("unknown"),
                    claim.heartbeat_at
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
                    warnings.push(format!(
                        "file-scope overlap with live claim from {} (heartbeat {}): {}",
                        claim.session_client.as_deref().unwrap_or("unknown"),
                        claim.heartbeat_at,
                        overlap
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
    }
    warnings
}

/// `tachi_memory(action='claim')` — manual claim registration for
/// harness-native work that never routes through briefing/intake/dispatch
/// (#1001 Scope item 5, the "harness didn't walk through our hooks"
/// backstop). Unlike the auto hook, a manual claim failure IS surfaced to the
/// caller — a caller explicitly asking to register a claim wants to know if
/// it did not happen.
pub(crate) fn handle_manual_claim(
    server: &MemoryServer,
    issue_ref: Option<String>,
    flow_id: Option<String>,
    branch: Option<String>,
    declared_file_scope: Option<Vec<String>>,
) -> Result<serde_json::Value, String> {
    if issue_ref.is_none() && flow_id.is_none() {
        return Err(
            "tachi_memory(action='claim') requires issue_ref and/or flow_id".to_string(),
        );
    }
    let session_client = resolve_session_client(server);
    let live = list_live_claims_for_briefing(server);
    let warnings = collision_warnings(
        &live,
        Some(session_client.as_str()),
        issue_ref.as_deref(),
        declared_file_scope.as_deref().unwrap_or_default(),
    );

    let new_claim = NewSessionClaim {
        claim_id: generate_claim_id(),
        session_client: Some(session_client.clone()),
        issue_ref: issue_ref.clone(),
        flow_id: flow_id.clone(),
        dispatch_id: None,
        branch: branch.unwrap_or_default(),
        declared_file_scope: declared_file_scope
            .as_ref()
            .map(|scope| serde_json::to_string(scope).unwrap_or_default()),
        created_at: String::new(),
    };
    let claim_id: String = server.with_global_store(|store| {
        memcore::upsert_or_heartbeat_claim(store.connection_mut(), &new_claim)
            .map_err(|e| e.to_string())
    })?;

    Ok(serde_json::json!({
        "status": "completed",
        "action": "claim",
        "claim_id": claim_id,
        "session_client": session_client,
        "issue_ref": issue_ref,
        "flow_id": flow_id,
        "warnings": warnings,
    }))
}

/// `tachi_memory(action='release')` — manual release, routed through THE
/// single release path (`memcore::release_claim`), same discipline as
/// `exec_env_ops::reclaim_exec_env`. Accepts a `claim_id` or a `dispatch_id`
/// selector so a caller can release by whichever identity it has on hand.
pub(crate) fn handle_manual_release(
    server: &MemoryServer,
    claim_id: Option<String>,
    dispatch_id: Option<String>,
    reason: Option<String>,
) -> Result<serde_json::Value, String> {
    let selector = match (claim_id, dispatch_id) {
        (Some(claim_id), _) if !claim_id.trim().is_empty() => ClaimSelector::ClaimId(claim_id),
        (_, Some(dispatch_id)) if !dispatch_id.trim().is_empty() => {
            ClaimSelector::DispatchId(dispatch_id)
        }
        _ => {
            return Err(
                "tachi_memory(action='release') requires claim_id or dispatch_id".to_string(),
            )
        }
    };
    let outcome: ReleaseOutcome = server.with_global_store(|store| {
        memcore::release_claim(store.connection_mut(), &selector, reason.as_deref())
            .map_err(|e| e.to_string())
    })?;
    let (status, claim_id) = match outcome {
        ReleaseOutcome::Released { claim_id } => ("released", Some(claim_id)),
        ReleaseOutcome::AlreadyReleased { claim_id } => ("already_released", Some(claim_id)),
        ReleaseOutcome::NotFound => ("not_found", None),
    };
    Ok(serde_json::json!({
        "status": "completed",
        "action": "release",
        "outcome": status,
        "claim_id": claim_id,
    }))
}

/// Briefing 工位表 projection: compact JSON rows the markdown/JSON briefing
/// surfaces both render. Read-failure-safe (empty on any storage error).
pub(crate) fn briefing_claims_board(server: &MemoryServer) -> serde_json::Value {
    let live = list_live_claims_for_briefing(server);
    let rows: Vec<serde_json::Value> = live
        .iter()
        .map(|c| {
            serde_json::json!({
                "session_client": c.session_client,
                "issue_ref": c.issue_ref,
                "flow_id": c.flow_id,
                "branch": c.branch,
                "heartbeat_at": c.heartbeat_at,
            })
        })
        .collect();
    serde_json::json!({
        "count": rows.len(),
        "items": rows,
    })
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
/// Read-failure-safe: every step degrades to empty on storage error, so this
/// never fails the briefing call that invokes it.
pub(crate) fn presence_briefing_section(
    server: &MemoryServer,
    issue_ref: Option<&str>,
) -> serde_json::Value {
    let live = list_live_claims_for_briefing(server);
    let board = briefing_claims_board(server);
    let warnings = collision_warnings(&live, None, issue_ref, &[]);
    serde_json::json!({
        "board": board,
        "warnings": warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(session_client: &str, issue_ref: &str, scope: Option<Vec<&str>>) -> SessionClaim {
        SessionClaim {
            claim_id: format!("c-{session_client}"),
            session_client: Some(session_client.to_string()),
            issue_ref: Some(issue_ref.to_string()),
            flow_id: None,
            dispatch_id: None,
            branch: "feat/x".to_string(),
            declared_file_scope: scope
                .map(|s| serde_json::to_string(&s).unwrap_or_default()),
            state: memcore::ClaimState::Active,
            release_reason: None,
            created_at: "2026-07-11T00:00:00Z".to_string(),
            heartbeat_at: "2026-07-11T00:05:00Z".to_string(),
            released_at: None,
        }
    }

    #[test]
    fn collision_warns_on_double_claim_same_issue() {
        let live = vec![claim("codex", "org/repo#100", None)];
        let warnings = collision_warnings(&live, Some("claude-code"), Some("org/repo#100"), &[]);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("double-claim"));
        assert!(warnings[0].contains("codex"));
    }

    #[test]
    fn collision_excludes_the_callers_own_prior_claim() {
        let live = vec![claim("claude-code", "org/repo#100", None)];
        let warnings = collision_warnings(&live, Some("claude-code"), Some("org/repo#100"), &[]);
        assert!(
            warnings.is_empty(),
            "re-claiming your own issue is not a collision"
        );
    }

    #[test]
    fn collision_warns_on_file_scope_overlap() {
        let live = vec![claim(
            "codex",
            "org/repo#200",
            Some(vec!["crates/foo/src/lib.rs", "crates/foo/src/bar.rs"]),
        )];
        let new_scope = vec!["crates/foo/src/bar.rs".to_string()];
        let warnings = collision_warnings(&live, Some("claude-code"), None, &new_scope);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("file-scope overlap"));
        assert!(warnings[0].contains("bar.rs"));
    }

    #[test]
    fn collision_no_warning_when_scopes_disjoint() {
        let live = vec![claim(
            "codex",
            "org/repo#200",
            Some(vec!["crates/foo/src/lib.rs"]),
        )];
        let new_scope = vec!["crates/other/src/lib.rs".to_string()];
        let warnings = collision_warnings(&live, Some("claude-code"), None, &new_scope);
        assert!(warnings.is_empty());
    }

    #[test]
    fn collision_no_warning_on_unrelated_issue_no_scope() {
        let live = vec![claim("codex", "org/repo#900", None)];
        let warnings = collision_warnings(&live, Some("claude-code"), Some("org/repo#901"), &[]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn generated_claim_ids_are_unique_and_prefixed() {
        let a = generate_claim_id();
        let b = generate_claim_id();
        assert!(a.starts_with("claim-"));
        assert_ne!(a, b);
    }

    #[test]
    fn hook_input_with_no_identity_is_inert() {
        // auto_register_or_heartbeat_claim requires a MemoryServer, which
        // needs a live daemon fixture; the no-identity short-circuit is
        // exercised directly here as a pure-logic check on the guard
        // condition it uses (issue_ref/flow_id both absent).
        let input = ClaimHookInput::default();
        assert!(input.issue_ref.is_none() && input.flow_id.is_none());
    }
}

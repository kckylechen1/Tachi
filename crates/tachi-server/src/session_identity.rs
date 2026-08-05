//! # Security scope (read before trusting this as multi-tenant identity)
//!
//! C1 (`reject_unbound_cross_project_write`) closes ONE attack: an UNBOUND HTTP
//! direct-connect session (no `X-Tachi-Project` header) targeting
//! `project=victim` via a tool argument. It does NOT authenticate the
//! `X-Tachi-Project` header itself — a direct HTTP client that claims
//! `X-Tachi-Project: victim` at `initialize` still binds to victim's project,
//! because project binding only verifies the project DB file exists
//! (existence ≡ access). Full multi-tenant authorization (binding header claims
//! to an authenticated identity via mTLS / local-only / vault-ACL) is tracked in
//! #495 and is OUT OF SCOPE for the #809 fix. Until #495 lands, treat this
//! module as "unbound-write rejection", NOT a complete identity spine.

use rmcp::model::JsonObject;

pub(crate) const HEADER_PROFILE: &str = "x-tachi-profile";
pub(crate) const HEADER_CLIENT: &str = "x-tachi-client";
pub(crate) const HEADER_AGENT_IDENTITY: &str = "x-tachi-agent-identity";
pub(crate) const HEADER_PROJECT: &str = "x-tachi-project";
/// #1120 PR1: same shape as `HEADER_PROJECT`, but carries a filesystem path
/// (a git repo root, or any path beneath one) instead of an already-registered
/// project name. Lets a raw HTTP direct-connect client that only knows its own
/// cwd — not a pre-registered Tachi project name — declare it at session init;
/// the daemon derives the git root + project identity and auto-registers the
/// project DB on first contact instead of requiring a prior explicit
/// `tachi_init_project_db` call. See
/// `MemoryServer::resolve_or_register_workspace_root` for the resolution law
/// (evaluated against the DAEMON's own filesystem — only meaningful when
/// client and daemon share one, e.g. today's stdio-proxy -> localhost-daemon
/// topology).
pub(crate) const HEADER_WORKSPACE_ROOT: &str = "x-tachi-workspace-root";

/// #1251: per-call recursion-depth marker for the recursive-dispatch gate.
/// It rides the SAME proxy→daemon per-call header rail as [`HEADER_PROJECT`]
/// (injected in `cli_client::transport::call_daemon_tool_raw`, read back in
/// `server_handler::http_session_identity`), NOT process env. A dispatched
/// worker's `tachi serve` is a thin stdio proxy that forwards every tool call
/// over HTTP to the singleton shared daemon, so `handle_tachi_dispatch` runs
/// IN the daemon carrying the DAEMON's env (always depth 0). A gate that read
/// process env would therefore always see depth 0 — security theater. The
/// depth must travel per-call over the wire so the daemon reads the CALLER's
/// depth into that session's identity.
pub(crate) const HEADER_DISPATCH_DEPTH: &str = "x-tachi-dispatch-depth";

/// #1251: the process-env var a parent stamps onto a child worker's
/// `tachi serve` (see `dispatch_ops::mcp_config`). The child's stdio proxy
/// reads it back from its OWN process env and re-emits it as
/// [`HEADER_DISPATCH_DEPTH`] on every daemon call. It is also the authoritative
/// depth source for the CLI in-process dispatch path, where the process IS the
/// real caller (no daemon hop), so its own env is genuine — not the daemon-env
/// theater the header rail exists to avoid.
pub(crate) const ENV_DISPATCH_DEPTH: &str = "TACHI_DISPATCH_DEPTH";

/// #1251: hard ceiling on nested dispatch depth. A session already at (or, via
/// malformed-value saturation, beyond) this depth is refused BEFORE any run
/// directory is created. Depth accounting: leader = 0, each child = parent + 1.
/// With a limit of 3, sessions at depth 0/1/2 may dispatch (minting children at
/// depth 1/2/3); a depth-3 worker can no longer dispatch — a bounded fan-out
/// that stops a runaway self-dispatch loop from exhausting the daemon.
pub(crate) const MAX_DISPATCH_DEPTH: u32 = 3;

pub(crate) const META_PROFILE: &str = "tachiProfile";
pub(crate) const META_CLIENT: &str = "tachiClient";
pub(crate) const META_AGENT_IDENTITY: &str = "tachiAgentIdentity";
pub(crate) const META_PROJECT: &str = "tachiProject";
/// `_meta` twin of [`HEADER_WORKSPACE_ROOT`] for transports that carry MCP
/// `initialize._meta` instead of (or in addition to) HTTP headers.
pub(crate) const META_WORKSPACE_ROOT: &str = "tachiWorkspaceRoot";

/// #1041 F2: wire key stamped onto the raw tool-call arguments (never a
/// caller-facing schema field — hidden via `#[schemars(skip)]` on every
/// params struct that carries it) to record whether `project` reflects the
/// CALLER's own explicit placement decision, versus a value this function
/// injected below because the transport (a bound stdio proxy or HTTP
/// direct-connect session) defaulted it. Before this signal existed, the
/// write-affinity gate (`save_memory/write_affinity.rs`) treated
/// `named_project.is_some()` as proof of deliberate intent — but bound
/// sessions unconditionally inject the session project onto every
/// project-defaulting write tool (see the tail of this function), so the
/// ordinary ambiguous-default save (the exact case #1041 S1 exists to
/// catch) always arrived at the gate looking "explicit" and skipped it
/// entirely.
///
/// #1041 B1 (codex round-4, real escalation — closed): this key is
/// deserialized straight off the wire into `project_explicit`
/// (`#[serde(rename = "__tachi_project_explicit")]` — `#[schemars(skip)]`
/// only hides it from the *published* tool schema, it does not stop a raw
/// client from sending the field directly). A client that sends an
/// explicit, bound-matching `project=` alongside a forged
/// `__tachi_project_explicit: false` used to have that forged `false`
/// preserved (see the now-removed `mark_project_explicit_unless_already_
/// defaulted`), making a genuinely-explicit write look like a
/// transport-injected default. `write_affinity.rs`'s S1 gate only
/// re-evaluates domain routing for that "default" case — so the forged
/// marker let a write whose caller explicitly pinned it to project X get
/// silently domain-rerouted into a *different*, already-mounted project Y,
/// a target `enforce_session_project` never validated at all (no identity
/// match check, no cross-binding check — only `write_affinity`'s
/// `project_exists` mount check runs against Y). The fix: the marker is
/// never read back off the wire and never "preserved" across hops. See
/// [`EnforcementRole`] for how the double-hop topology now avoids needing
/// to trust an incoming wire value for this key at all.
pub(crate) const PROJECT_EXPLICIT_MARKER: &str = "__tachi_project_explicit";

/// Which hop of the (possible) stdio-proxy -> daemon-HTTP double hop is
/// calling [`enforce_session_project`]. #1041 B1: introduced so the marker
/// this function stamps is *computed*, never *trusted off the wire* —
/// eliminating the need to "preserve an incoming false across hops" (the
/// mechanism a forged wire value used to hide behind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnforcementRole {
    /// A non-injecting pre-check run before forwarding a request over an
    /// internal transport hop (today: the stdio proxy, before it forwards to
    /// the daemon over HTTP in `prepare_proxy_tool_call`). Still validates,
    /// rejects, and normalizes a caller-explicit `project=` exactly like
    /// `Authoritative` (so bad requests fail fast, before a network
    /// round-trip) — the ONLY thing it never does is inject a *default*
    /// `project` when the caller omitted one. That decision, and the
    /// `PROJECT_EXPLICIT_MARKER = false` that goes with it, is deferred
    /// entirely to the `Authoritative` hop. A `Preflight` hop must never be
    /// the only hop enforcing a given tool call.
    Preflight,
    /// The single, sole point that may inject a default `project` for an
    /// absent one (today: the daemon's own HTTP `call_tool` in
    /// `server_handler.rs`, and the CLI's direct daemon-only path, which has
    /// no `Preflight` hop at all). Because `Preflight` never injects, by the
    /// time an `Authoritative` call sees `project` present in the arguments,
    /// it is unambiguously genuine caller intent — either supplied directly
    /// to this hop, or forwarded unmodified by an earlier `Preflight` hop.
    /// Either way the marker is stamped `true`, unconditionally; an absent
    /// `project` is injected here and stamped `false`, unconditionally. No
    /// incoming wire value for the marker key is ever read.
    Authoritative,
}

/// Stamp [`PROJECT_EXPLICIT_MARKER`] onto `args`, unconditionally overwriting
/// any incoming wire value (never read one back — see the marker's own doc
/// for why that must stay true). This is the shared half of
/// `enforce_session_project`'s two stamping sites below, factored out so a
/// caller that has NO bound session to enforce against — the CLI's
/// in-process dispatch fallback (`bootstrap::cli_tool::tool_dispatch::
/// dispatch_cli_tool`, which never runs `enforce_session_project` at all
/// because there is no daemon/session in that branch) — can still produce
/// the identical caller-explicit-vs-transport-default signal the
/// write-affinity gate (`memory_search_ops::save_memory::write_affinity`)
/// depends on, instead of hand-rolling a second copy of this semantics.
pub(crate) fn stamp_project_explicit_marker(args: &mut JsonObject, explicit: bool) {
    args.insert(
        PROJECT_EXPLICIT_MARKER.to_string(),
        serde_json::json!(explicit),
    );
}

pub(crate) fn enforce_session_project(
    tool_name: &str,
    arguments: &mut Option<JsonObject>,
    project: &str,
    transport_label: &str,
    role: EnforcementRole,
) -> Result<(), rmcp::ErrorData> {
    enforce_session_project_impl(None, tool_name, arguments, project, transport_label, role)
}

pub(crate) fn enforce_server_session_project(
    server: &crate::MemoryServer,
    tool_name: &str,
    arguments: &mut Option<JsonObject>,
    project: &str,
    transport_label: &str,
    role: EnforcementRole,
) -> Result<(), rmcp::ErrorData> {
    enforce_session_project_impl(
        Some(server),
        tool_name,
        arguments,
        project,
        transport_label,
        role,
    )
}

fn enforce_session_project_impl(
    server: Option<&crate::MemoryServer>,
    tool_name: &str,
    arguments: &mut Option<JsonObject>,
    project: &str,
    transport_label: &str,
    role: EnforcementRole,
) -> Result<(), rmcp::ErrorData> {
    let args = arguments.get_or_insert_with(serde_json::Map::new);
    if let Some(explicit_project) = args.get("project") {
        let Some(requested_alias) = explicit_project.as_str().map(str::to_string) else {
            let requested = explicit_project.to_string();
            return Err(project_binding_mismatch(
                transport_label,
                &requested,
                project,
                Some("project must be a string"),
            ));
        };
        // #1041 B1: unconditionally genuine — never preserve/trust whatever
        // value the wire already had for this key. See [`EnforcementRole`]
        // for why this is safe regardless of which hop is calling.
        stamp_project_explicit_marker(args, true);
        let resolve_identity = |name: &str| match server {
            Some(server) => server.resolve_server_named_project_db_identity(name),
            None => crate::MemoryServer::resolve_named_project_db_identity(name),
        };
        let requested_identity = resolve_identity(&requested_alias);
        let bound_identity = resolve_identity(project);

        match (requested_identity, bound_identity) {
            (Ok(requested_db), Ok(bound_db)) if requested_db == bound_db => {
                // The immutable session identity wins even when the caller used
                // a legacy/simplified alias for the same physical DB. This also
                // prevents downstream code from opening the same DB under a
                // second label.
                args.insert("project".to_string(), serde_json::json!(project));
                return Ok(());
            }
            (Ok(_), Ok(_)) if explicit_project_can_cross_binding(tool_name, args) => {
                // #737 read/write asymmetry: a resolvable different project is
                // still allowed only for the established read-only actions.
                return Ok(());
            }
            (Ok(_), Ok(_)) => {
                return Err(project_binding_mismatch(
                    transport_label,
                    &requested_alias,
                    project,
                    Some("requested alias resolves to a different canonical database"),
                ));
            }
            (Err(err), _) => {
                return Err(project_binding_mismatch(
                    transport_label,
                    &requested_alias,
                    project,
                    Some(&format!("requested alias resolution failed closed: {err}")),
                ));
            }
            (_, Err(err)) => {
                return Err(project_binding_mismatch(
                    transport_label,
                    &requested_alias,
                    project,
                    Some(&format!("bound identity resolution failed closed: {err}")),
                ));
            }
        }
    }
    // #1041 B1: `Preflight` never injects a default — that decision (and the
    // `false` marker that goes with it) is deferred entirely to whichever
    // `Authoritative` hop eventually sees this request, so there is exactly
    // one place a `project` can go from absent to present, and no wire value
    // for the marker to preserve or forge across the gap.
    if role == EnforcementRole::Preflight {
        return Ok(());
    }
    if !project_defaults_to_bound_project(tool_name, args) {
        return Ok(());
    }
    if tool_name == "tachi_memory"
        && args
            .get("action")
            .and_then(|value| value.as_str())
            .map(|action| !tachi_memory_action_defaults_to_project(action))
            .unwrap_or(false)
    {
        return Ok(());
    }
    if args
        .get("scope")
        .and_then(|value| value.as_str())
        .is_some_and(|scope| scope.eq_ignore_ascii_case("global"))
    {
        return Ok(());
    }
    // #1041 F2: this is the transport-injected default itself, not a caller
    // decision — always stamp `false` (unconditionally; `project` cannot
    // already be present here, this branch only runs when it was absent).
    stamp_project_explicit_marker(args, false);
    args.insert("project".to_string(), serde_json::json!(project));
    Ok(())
}

fn project_binding_mismatch(
    transport_label: &str,
    requested_alias: &str,
    effective_bound_identity: &str,
    reason: Option<&str>,
) -> rmcp::ErrorData {
    let reason = reason
        .map(|reason| format!(" Reason: {reason}."))
        .unwrap_or_default();
    rmcp::ErrorData::invalid_params(
        format!(
            "{transport_label} project binding mismatch: requested alias '{requested_alias}' is not the effective bound identity '{effective_bound_identity}'.{reason} \
repair: omit the project parameter (session binding wins). Same-DB aliases are accepted and normalized to '{effective_bound_identity}'; other-project writes and destructive actions remain forbidden"
        ),
        None,
    )
}

/// Returns true when an explicit `project` param may differ from the session
/// binding. Protects write isolation only: daemon-side reads use read-only opens
/// and do not threaten single-writer discipline.
pub(crate) fn explicit_project_can_cross_binding(tool_name: &str, args: &JsonObject) -> bool {
    match tool_name {
        "search_memory"
        | "find_similar_memory"
        | "get_memory"
        | "list_memories"
        | "tachi_search" => true,
        "tachi_memory" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_memory_action_allows_cross_project_read),
        "tachi_wiki" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_wiki_action_allows_cross_project_read),
        "tachi_event" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_event_action_allows_cross_project_read),
        // #1426: `recall_simulate` kept its read-only cross-project standing
        // when it moved off `tachi_memory`. Nothing else on `tachi_tune` may
        // cross a session binding: the route/recall review and apply arms
        // write proposal rows and config.env, and `tachi_task` (route
        // tuning's old home) never allowed cross-binding at all.
        "tachi_tune" => args
            .get("action")
            .and_then(|value| value.as_str())
            .is_some_and(tachi_tune_action_allows_cross_project_read),
        _ => false,
    }
}

pub(crate) fn project_defaults_to_bound_project(tool_name: &str, args: &JsonObject) -> bool {
    if tool_name == "tachi_memory" {
        return args
            .get("action")
            .and_then(|value| value.as_str())
            .is_none_or(tachi_memory_action_defaults_to_project);
    }
    matches!(
        tool_name,
        "search_memory"
            | "find_similar_memory"
            | "get_memory"
            | "list_memories"
            | "archive_memory"
            | "save_memory"
            | "remember"
            | "ingest_event"
            | "extract_facts"
            | "tachi_search"
            | "tachi_save"
            | "tachi_event"
            | "tachi_domain_adapter"
            | "tachi_task"
            // #1426: route tuning defaulted to the bound project as part of
            // `tachi_task`, and all four recall-tuning actions defaulted to it
            // under `tachi_memory`. Every `tachi_tune` action therefore
            // defaults, exactly as both halves did before the move — a new
            // facade gets NO project defaulting without this entry.
            | "tachi_tune"
            | "tachi_verify"
            | "tachi_gh"
            | "tachi_wiki"
            | "wiki_write"
            | "tachi_wiki_write"
            // #1114 (codex round-1 B2 fix): `capture_session` omitting
            // `project=` used to fall all the way through
            // `resolve_capture_target` to `resolve_write_scope`, which
            // resolves the DAEMON's own static `has_project_db()` binding —
            // NOT the calling HTTP/stdio session's own bound project
            // (`self.session_project()`, which can differ per session on a
            // daemon serving multiple bound sessions). Without this entry,
            // a session bound to project B calling `capture_session`
            // without `project=` silently captured rows into the daemon's
            // project A. Adding it here makes `enforce_session_project`
            // inject the SESSION's own bound project (with
            // `project_explicit: false`) the same way every other
            // project-defaulting write tool already gets.
            | "capture_session"
            // #1114 (codex round-2 item 2 fix): `compact_session_memory`
            // calls the SAME `resolve_capture_target` as `capture_session`
            // and has the identical exposure — a session bound to project B
            // compacting through a daemon whose own static binding is
            // project A would silently land the compacted rollup in A.
            | "compact_session_memory"
    ) || tool_name == "tachi_memory"
}

fn tachi_memory_action_defaults_to_project(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "alerts"
            | "ask"
            | "briefing"
            | "checkpoint"
            | "consolidate"
            | "delete"
            | "extract_facts"
            | "get"
            | "ingest"
            | "ingest_source"
            | "pattern_feedback"
            | "progress"
            | "readiness"
            | "save"
            | "search"
    )
}

fn tachi_memory_action_allows_cross_project_read(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "alerts" | "ask" | "briefing" | "consolidate" | "get" | "readiness" | "search"
    )
}

/// #1426: the `tachi_memory` half of the tuning surface — only
/// `recall_simulate` was a read-only cross-project case there, and it replays
/// searches without mutating access counters, so it keeps that standing.
fn tachi_tune_action_allows_cross_project_read(action: &str) -> bool {
    matches!(action.to_ascii_lowercase().as_str(), "recall_simulate")
}

fn tachi_event_action_allows_cross_project_read(action: &str) -> bool {
    matches!(action.to_ascii_lowercase().as_str(), "metrics" | "query")
}

fn tachi_wiki_action_allows_cross_project_read(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "browse" | "read" | "search"
    )
}

/// Reject explicit cross-project targeting from an UNBOUND session when the
/// tool/action is not a read-only cross-project case. Bound sessions are
/// handled by `enforce_session_project`. An unbound HTTP direct-connect session
/// has no declared tenant, so an explicit `project=` on a mutating tool is a
/// potential cross-tenant write and must be rejected. (C1 fix.)
///
/// Invariant protected: single-writer project isolation — an unbound session
/// must not be able to route a write into an arbitrary project's DB. Read-only
/// cross-project cases (handled by `explicit_project_can_cross_binding`) do not
/// threaten single-writer discipline and remain allowed; this guard checks both
/// sides so it does not over-reach into legitimate reads.
pub(crate) fn reject_unbound_cross_project_write(
    tool_name: &str,
    arguments: &Option<JsonObject>,
    bound_project: Option<&str>,
    transport_label: &str,
) -> Result<(), rmcp::ErrorData> {
    if bound_project.is_some() {
        return Ok(());
    }
    let Some(args) = arguments.as_ref() else {
        return Ok(());
    };
    let Some(explicit) = args.get("project").and_then(|v| v.as_str()) else {
        return Ok(());
    };
    if explicit_project_can_cross_binding(tool_name, args) {
        return Ok(());
    }
    Err(rmcp::ErrorData::invalid_params(
        format!(
            "{transport_label} session is not bound to a project; refusing explicit project='{explicit}' on tool '{tool_name}' (cross-project writes require a bound session — send X-Tachi-Project at initialize)"
        ),
        None,
    ))
}

pub(crate) fn normalize_identity_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

pub(crate) fn valid_agent_identity_assertion(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

/// #1251: resolve a caller's dispatch recursion depth from the (optional) wire
/// marker carried by [`HEADER_DISPATCH_DEPTH`] / [`ENV_DISPATCH_DEPTH`]. The
/// accounting is fail-closed by construction:
/// - absent marker → `0` (a leader session — the dispatch plumbing stamps a
///   depth onto every real child, so absence genuinely means "top of tree",
///   not "child that dropped its marker"),
/// - a present, well-formed non-negative integer → honored verbatim,
/// - a present but malformed / negative (`"-1"`) / overflowing value →
///   saturates to `limit`, so it can only ever FAIL the gate, never
///   parse-error into an implicit "allow".
pub(crate) fn resolve_dispatch_depth(raw: Option<&str>, limit: u32) -> u32 {
    match raw {
        None => 0,
        // `u32::from_str` rejects a leading `-`, non-digits, and anything past
        // `u32::MAX`; every one of those saturates to the limit and fails
        // closed rather than being misread as a small (or zero) depth.
        Some(marker) => marker.trim().parse::<u32>().unwrap_or(limit),
    }
}

/// #1251: the pure fail-closed recursion gate. A dispatch requested by a
/// session already at `depth` is refused once `depth >= limit`. Kept a free
/// function (no I/O, no session handle) so the depth math is unit-testable in
/// isolation from the header/env plumbing that feeds it, and so the single
/// comparison that decides "allow vs refuse" lives in exactly one place.
///
/// #1251 v1 residual (owner-accepted, matches the existing worker
/// self-report trust boundary — same class as `TACHI_AGENT_SEAT`): this gate
/// is CALLER-ASSERTED. It defends against ACCIDENTAL unbounded recursion via
/// the normal MCP-dispatch path (a leader/worker that keeps calling
/// `tachi_staff(action='start')` on itself). It does NOT defend against a
/// DELIBERATE worker choosing to bypass the marker — e.g. invoking the raw
/// `tachi task` CLI outside the env-stamped path, or a forged
/// `X-Tachi-Dispatch-Depth` header on a direct HTTP connection — because
/// nothing here binds the depth claim to an authenticated capability. A
/// server-side capability-token binding (the depth carried in a signed/opaque
/// token minted by the parent, unforgeable by the child) is the follow-up
/// hardening, tracked as a separate issue; it is explicitly OUT OF SCOPE for
/// this v1 accidental-runaway defense.
pub(crate) fn enforce_dispatch_depth(depth: u32, limit: u32) -> Result<(), String> {
    if depth >= limit {
        return Err(format!(
            "recursive dispatch depth limit reached: caller is at dispatch depth {depth}, which \
             meets or exceeds MAX_DISPATCH_DEPTH ({limit}); refusing to spawn a deeper child \
             before any run directory is created (fail-closed recursion gate, #1251)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn args(map: serde_json::Map<String, serde_json::Value>) -> Option<JsonObject> {
        Some(map)
    }

    fn map_from(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    use crate::test_support::EnvRestore;

    fn with_test_home<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _guard = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let temp = tempfile::tempdir().expect("tempdir");
        let tachi_home = temp.path().join("home");
        std::fs::create_dir_all(&tachi_home).expect("tachi home");
        let _tachi_home = EnvRestore::set_path("TACHI_HOME", &tachi_home);
        let _sigil_home = EnvRestore::remove("SIGIL_HOME");
        let _app_home = EnvRestore::remove("TACHI_APP_HOME");

        f(&tachi_home)
    }

    fn repo_db(root: &Path) -> PathBuf {
        let db = root.join(".tachi/memory.db");
        std::fs::create_dir_all(db.parent().expect("db parent")).expect("create db parent");
        std::fs::write(&db, b"identity-only fixture").expect("write db fixture");
        db
    }

    fn write_manifest(tachi_home: &Path, db_paths: &[&Path]) -> Vec<u8> {
        let entries = db_paths
            .iter()
            .map(|db_path| {
                json!({
                    "path": db_path.to_string_lossy(),
                    "role": "project",
                    "owner": "project:test",
                    "schema_kind": "tachi",
                    "vec_enabled": false,
                    "allow_write": true,
                    "last_doctor_at": "1970-01-01T00:00:00Z",
                    "last_classification": "healthy",
                    "scope_hint": "test"
                })
            })
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "generated_at": "1970-01-01T00:00:00Z",
            "_comment": "identity test",
            "dbs": entries
        }))
        .expect("serialize manifest");
        std::fs::write(tachi_home.join("manifest.json"), &bytes).expect("write manifest");
        bytes
    }

    #[test]
    fn bound_session_rejects_cross_project_write() {
        let mut arguments = args(map_from(&[
            ("action", json!("save")),
            ("project", json!("other")),
            ("text", json!("nope")),
        ]));
        let err = enforce_session_project(
            "tachi_memory",
            &mut arguments,
            "sigil",
            "HTTP direct-connect",
            EnforcementRole::Authoritative,
        )
        .expect_err("cross-project write must fail");
        assert!(
            err.message.contains("project binding mismatch"),
            "got: {}",
            err.message
        );
        // #925: bare mismatch is agent-hostile — must name the repair.
        assert!(
            err.message.contains("repair:")
                && err.message.contains("omit the project parameter")
                && err.message.contains("effective bound identity 'sigil'"),
            "mismatch must include repair hint, got: {}",
            err.message
        );
    }

    #[test]
    fn bound_session_allows_cross_project_read_search() {
        with_test_home(|tachi_home| {
            let sigil = tachi_home.join("projects/sigil/memory.db");
            let other = tachi_home.join("projects/other/memory.db");
            std::fs::create_dir_all(sigil.parent().expect("sigil parent")).expect("sigil parent");
            std::fs::create_dir_all(other.parent().expect("other parent")).expect("other parent");
            std::fs::write(&sigil, b"sigil").expect("sigil db");
            std::fs::write(&other, b"other").expect("other db");

            let mut arguments = args(map_from(&[
                ("action", json!("search")),
                ("project", json!("other")),
                ("query", json!("hello")),
            ]));
            enforce_session_project(
                "tachi_memory",
                &mut arguments,
                "sigil",
                "HTTP direct-connect",
                EnforcementRole::Authoritative,
            )
            .expect("cross-project read must pass");
            assert_eq!(
                arguments
                    .as_ref()
                    .and_then(|a| a.get("project"))
                    .and_then(|v| v.as_str()),
                Some("other")
            );
        });
    }

    #[test]
    fn bound_session_normalizes_hashed_and_legacy_aliases_in_either_direction() {
        with_test_home(|tachi_home| {
            let repo = tachi_home.join("repos/Sigil");
            let db = repo_db(&repo);
            write_manifest(tachi_home, &[&db]);
            let hashed = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("hashed alias");
            let legacy =
                crate::path_utils::plan_c_legacy_dir_name_from_root(&repo).expect("legacy alias");

            for (bound, requested) in [(&hashed, &legacy), (&legacy, &hashed)] {
                let mut arguments = args(map_from(&[
                    ("action", json!("save")),
                    ("project", json!(requested)),
                    ("text", json!("same physical DB")),
                ]));
                enforce_session_project(
                    "tachi_memory",
                    &mut arguments,
                    bound,
                    "HTTP direct-connect",
                    EnforcementRole::Authoritative,
                )
                .unwrap_or_else(|err| {
                    panic!("{requested} should normalize to bound {bound}: {err}")
                });
                assert_eq!(
                    arguments
                        .as_ref()
                        .and_then(|a| a.get("project"))
                        .and_then(|v| v.as_str()),
                    Some(bound.as_str()),
                    "effective identity must remain the immutable binding"
                );
            }
        });
    }

    #[test]
    fn bound_session_rejects_ambiguous_legacy_alias_for_equal_repo_basenames() {
        with_test_home(|tachi_home| {
            let repo_a = tachi_home.join("workspace-a/Sigil");
            let repo_b = tachi_home.join("workspace-b/Sigil");
            let db_a = repo_db(&repo_a);
            let db_b = repo_db(&repo_b);
            write_manifest(tachi_home, &[&db_a, &db_b]);
            let bound = crate::path_utils::plan_c_dir_name_from_root(&repo_a).expect("bound alias");
            let mut arguments = args(map_from(&[
                ("action", json!("save")),
                ("project", json!("Sigil")),
                ("text", json!("must not guess")),
            ]));

            let err = enforce_session_project(
                "tachi_memory",
                &mut arguments,
                &bound,
                "stdio proxy",
                EnforcementRole::Preflight,
            )
            .expect_err("duplicate legacy basename must be ambiguous");
            assert!(err.message.contains("requested alias 'Sigil'"));
            assert!(err
                .message
                .contains(&format!("effective bound identity '{bound}'")));
            assert!(err.message.contains("ambiguous"), "got: {}", err.message);

            let other_hashed =
                crate::path_utils::plan_c_dir_name_from_root(&repo_b).expect("other hashed alias");
            let mut arguments = args(map_from(&[
                ("action", json!("save")),
                ("project", json!(&other_hashed)),
                ("text", json!("same basename is not same DB")),
            ]));
            let err = enforce_session_project(
                "tachi_memory",
                &mut arguments,
                &bound,
                "stdio proxy",
                EnforcementRole::Preflight,
            )
            .expect_err("distinct hashed aliases with equal basenames must remain isolated");
            assert!(
                err.message.contains("different canonical database"),
                "got: {}",
                err.message
            );
        });
    }

    #[test]
    fn bound_session_rejects_genuinely_different_write_and_destructive_aliases() {
        with_test_home(|tachi_home| {
            let sigil_repo = tachi_home.join("repos/Sigil");
            let quant_repo = tachi_home.join("repos/Quant");
            let sigil_db = repo_db(&sigil_repo);
            let quant_db = repo_db(&quant_repo);
            write_manifest(tachi_home, &[&sigil_db, &quant_db]);
            let bound =
                crate::path_utils::plan_c_dir_name_from_root(&sigil_repo).expect("bound alias");
            let other =
                crate::path_utils::plan_c_dir_name_from_root(&quant_repo).expect("other alias");

            for action in ["save", "delete"] {
                let mut arguments = args(map_from(&[
                    ("action", json!(action)),
                    ("project", json!(&other)),
                    ("text", json!("must stay isolated")),
                ]));
                let err = enforce_session_project(
                    "tachi_memory",
                    &mut arguments,
                    &bound,
                    "HTTP direct-connect",
                    EnforcementRole::Authoritative,
                )
                .expect_err("different-project mutation must fail");
                assert!(
                    err.message.contains("different canonical database"),
                    "got: {}",
                    err.message
                );
            }
        });
    }

    #[test]
    fn unresolvable_alias_rejection_has_zero_filesystem_side_effects() {
        with_test_home(|tachi_home| {
            let repo = tachi_home.join("repos/Sigil");
            let db = repo_db(&repo);
            let manifest_before = write_manifest(tachi_home, &[&db]);
            let bound = crate::path_utils::plan_c_dir_name_from_root(&repo).expect("bound alias");
            let projects = tachi_home.join("projects");
            let broken = projects.join("broken/memory.db");
            std::fs::create_dir_all(broken.parent().expect("broken parent"))
                .expect("broken parent");
            #[cfg(unix)]
            std::os::unix::fs::symlink(tachi_home.join("missing/memory.db"), &broken)
                .expect("broken symlink");

            for alias in ["unknown", "broken"] {
                let mut arguments = args(map_from(&[
                    ("action", json!("save")),
                    ("project", json!(alias)),
                    ("text", json!("must not create")),
                ]));
                let err = enforce_session_project(
                    "tachi_memory",
                    &mut arguments,
                    &bound,
                    "stdio proxy",
                    EnforcementRole::Preflight,
                )
                .expect_err("unresolvable alias must fail closed");
                assert!(
                    err.message.contains("resolution failed closed"),
                    "got: {}",
                    err.message
                );
            }

            assert!(
                !projects.join("unknown").exists(),
                "unknown dir was created"
            );
            assert!(
                !tachi_home.join("missing").exists(),
                "broken target was repaired"
            );
            #[cfg(unix)]
            assert!(broken.is_symlink(), "broken alias must remain untouched");
            assert_eq!(
                std::fs::read(tachi_home.join("manifest.json")).expect("read manifest"),
                manifest_before,
                "validation must not alter the manifest"
            );
        });
    }

    #[test]
    fn manifest_resolution_failure_rejects_even_equal_alias_without_mutation() {
        with_test_home(|tachi_home| {
            let bound_db = tachi_home.join("projects/sigil/memory.db");
            std::fs::create_dir_all(bound_db.parent().expect("bound parent"))
                .expect("bound parent");
            std::fs::write(&bound_db, b"bound DB").expect("bound DB");
            let invalid_manifest = b"{ not valid manifest JSON";
            std::fs::write(tachi_home.join("manifest.json"), invalid_manifest)
                .expect("invalid manifest");
            let mut arguments = args(map_from(&[
                ("action", json!("save")),
                ("project", json!("sigil")),
                ("text", json!("must fail closed")),
            ]));

            let err = enforce_session_project(
                "tachi_memory",
                &mut arguments,
                "sigil",
                "HTTP direct-connect",
                EnforcementRole::Authoritative,
            )
            .expect_err("manifest parse failure must reject an otherwise equal alias");
            assert!(
                err.message.contains("manifest cannot be read")
                    && err.message.contains("failed closed"),
                "got: {}",
                err.message
            );
            assert_eq!(
                std::fs::read(tachi_home.join("manifest.json")).expect("manifest unchanged"),
                invalid_manifest
            );
            assert_eq!(std::fs::read(&bound_db).expect("DB unchanged"), b"bound DB");
        });
    }

    #[test]
    fn bound_session_injects_project_for_defaulting_write() {
        let mut arguments = args(map_from(&[
            ("action", json!("save")),
            ("text", json!("bound write")),
        ]));
        // #1041 B1: only the Authoritative hop may inject a default project.
        enforce_session_project(
            "tachi_memory",
            &mut arguments,
            "sigil",
            "HTTP direct-connect",
            EnforcementRole::Authoritative,
        )
        .expect("inject project");
        assert_eq!(
            arguments
                .as_ref()
                .and_then(|a| a.get("project"))
                .and_then(|v| v.as_str()),
            Some("sigil")
        );
    }

    /// #1041 B1: a `Preflight` hop (the stdio proxy, before it forwards to
    /// the daemon) must NEVER inject a default `project` for an absent one —
    /// that decision (and the marker that goes with it) is deferred entirely
    /// to whichever `Authoritative` hop the request eventually reaches. This
    /// is what makes the marker unforgeable: there is no wire round-trip
    /// carrying "hop 1 decided this was a default" for an attacker to spoof.
    #[test]
    fn preflight_hop_never_injects_a_default_project() {
        let mut arguments = args(map_from(&[
            ("action", json!("save")),
            ("text", json!("bound write, no project given")),
        ]));
        enforce_session_project(
            "tachi_memory",
            &mut arguments,
            "sigil",
            "stdio proxy",
            EnforcementRole::Preflight,
        )
        .expect("preflight passes through");
        let args = arguments.as_ref().expect("arguments present");
        assert!(
            args.get("project").is_none(),
            "Preflight must not inject a default project"
        );
        assert!(
            args.get(PROJECT_EXPLICIT_MARKER).is_none(),
            "Preflight must not stamp a marker for a project it never touched"
        );
    }

    /// #1041 B1 (dispatcher-required judgment test — already-safe path,
    /// nailed down): a forged `__tachi_project_explicit: true` alongside an
    /// OMITTED `project=` must not let the default-inject branch skip
    /// stamping `false`. This half of the marker was never wire-trusted even
    /// before the B1 fix (the inject branch always overwrote unconditionally)
    /// — this test exists so that invariant has explicit coverage, not just
    /// the explicit-project half (`forged_default_marker_on_explicit_project_is_overwritten`).
    #[test]
    fn forged_true_marker_on_defaulted_project_does_not_skip_the_gate() {
        let mut arguments = args(map_from(&[
            ("action", json!("save")),
            ("text", json!("bound write, no project given")),
            (PROJECT_EXPLICIT_MARKER, json!(true)),
        ]));
        enforce_session_project(
            "tachi_memory",
            &mut arguments,
            "sigil",
            "HTTP direct-connect",
            EnforcementRole::Authoritative,
        )
        .expect("inject project");
        assert_eq!(
            arguments
                .as_ref()
                .and_then(|a| a.get(PROJECT_EXPLICIT_MARKER))
                .and_then(|v| v.as_bool()),
            Some(false),
            "a forged `true` on an omitted project must not survive — the \
             injected default is always stamped false, unconditionally"
        );
    }

    // ── #1041 round-7: shared stamping helper ────────────────────────────────

    /// `stamp_project_explicit_marker` is the extracted half of
    /// `enforce_session_project`'s marker logic that a caller WITHOUT a
    /// bound session (the CLI in-process dispatch fallback) can also use —
    /// verify it round-trips both polarities and unconditionally overwrites
    /// any prior value, matching `enforce_session_project`'s own contract of
    /// never trusting/preserving a wire value for this key.
    #[test]
    fn stamp_project_explicit_marker_sets_both_polarities_and_overwrites() {
        let mut map = map_from(&[("project", json!("hapi"))]);
        stamp_project_explicit_marker(&mut map, true);
        assert_eq!(map.get(PROJECT_EXPLICIT_MARKER), Some(&json!(true)));

        stamp_project_explicit_marker(&mut map, false);
        assert_eq!(
            map.get(PROJECT_EXPLICIT_MARKER),
            Some(&json!(false)),
            "must overwrite a prior true, not preserve it"
        );
    }

    // ── #1041 F2: PROJECT_EXPLICIT_MARKER ────────────────────────────────────

    #[test]
    fn defaulted_project_is_stamped_not_explicit() {
        let mut arguments = args(map_from(&[
            ("action", json!("save")),
            ("text", json!("bound write, no project given")),
        ]));
        enforce_session_project(
            "tachi_memory",
            &mut arguments,
            "sigil",
            "HTTP direct-connect",
            EnforcementRole::Authoritative,
        )
        .expect("inject project");
        assert_eq!(
            arguments
                .as_ref()
                .and_then(|a| a.get(PROJECT_EXPLICIT_MARKER))
                .and_then(|v| v.as_bool()),
            Some(false),
            "a transport-injected default must be stamped NOT explicit"
        );
    }

    /// #1114 codex round-1 B2 discriminating test: `capture_session` must
    /// get the SAME session-bound-project injection every other
    /// project-defaulting write tool gets. Before the B2 fix,
    /// `project_defaults_to_bound_project` did not list `capture_session`,
    /// so `enforce_session_project` returned early without touching
    /// `args["project"]` at all — a session bound to a DIFFERENT project
    /// than whatever the daemon's own static `has_project_db()` happens to
    /// be would have its capture rows silently fall through
    /// `resolve_capture_target`/`resolve_write_scope` into the daemon's
    /// default project instead of the session's own bound one.
    #[test]
    fn capture_session_omitted_project_is_injected_from_session_binding() {
        let mut arguments = args(map_from(&[
            ("conversation_id", json!("c1")),
            ("turn_id", json!("t1")),
            ("agent_id", json!("agent")),
        ]));
        enforce_session_project(
            "capture_session",
            &mut arguments,
            "project-b",
            "HTTP direct-connect",
            EnforcementRole::Authoritative,
        )
        .expect("inject session-bound project");
        assert_eq!(
            arguments
                .as_ref()
                .and_then(|a| a.get("project"))
                .and_then(|v| v.as_str()),
            Some("project-b"),
            "capture_session omitting project= must get the SESSION's own \
             bound project injected, not fall through to the daemon's static \
             default further down the call chain"
        );
        assert_eq!(
            arguments
                .as_ref()
                .and_then(|a| a.get(PROJECT_EXPLICIT_MARKER))
                .and_then(|v| v.as_bool()),
            Some(false),
            "the injected value is a transport default, not a caller decision"
        );
    }

    /// #1114 codex round-2 item 2 discriminating test: `compact_session_memory`
    /// has the SAME `resolve_capture_target` exposure `capture_session` does
    /// — same fix, same allowlist entry.
    #[test]
    fn compact_session_memory_omitted_project_is_injected_from_session_binding() {
        let mut arguments = args(map_from(&[
            ("agent_id", json!("agent")),
            ("conversation_id", json!("c1")),
            ("window_id", json!("w1")),
        ]));
        enforce_session_project(
            "compact_session_memory",
            &mut arguments,
            "project-b",
            "HTTP direct-connect",
            EnforcementRole::Authoritative,
        )
        .expect("inject session-bound project");
        assert_eq!(
            arguments
                .as_ref()
                .and_then(|a| a.get("project"))
                .and_then(|v| v.as_str()),
            Some("project-b"),
            "compact_session_memory omitting project= must get the SESSION's \
             own bound project injected"
        );
        assert_eq!(
            arguments
                .as_ref()
                .and_then(|a| a.get(PROJECT_EXPLICIT_MARKER))
                .and_then(|v| v.as_bool()),
            Some(false),
            "the injected value is a transport default, not a caller decision"
        );
    }

    #[test]
    fn genuinely_explicit_project_is_stamped_explicit() {
        // #1041 round-3 fixture fix: an explicit `project=` (equal to the
        // bound identity) resolves through `resolve_named_project_db_identity`,
        // which fails closed when the named project has no DB on disk.
        // Without `with_test_home` + a seeded `projects/sigil/memory.db`,
        // this only "passed" by coincidence on a dev box that happened to
        // have a real `sigil` project registered — it fails closed
        // ("Project 'sigil' not found") in a clean environment/CI.
        with_test_home(|tachi_home| {
            let sigil = tachi_home.join("projects/sigil/memory.db");
            std::fs::create_dir_all(sigil.parent().expect("sigil parent")).expect("sigil parent");
            std::fs::write(&sigil, b"sigil").expect("sigil db");

            let mut arguments = args(map_from(&[
                ("action", json!("save")),
                ("project", json!("sigil")),
                ("text", json!("caller named its own bound project")),
            ]));
            enforce_session_project(
                "tachi_memory",
                &mut arguments,
                "sigil",
                "stdio proxy",
                EnforcementRole::Preflight,
            )
            .expect("same-DB alias normalizes");
            assert_eq!(
                arguments
                    .as_ref()
                    .and_then(|a| a.get(PROJECT_EXPLICIT_MARKER))
                    .and_then(|v| v.as_bool()),
                Some(true),
                "a caller-supplied project=, even if it resolves to the bound \
                 identity, is still a genuine explicit decision"
            );
        });
    }

    /// #1041 B1 core regression (codex round-4): a raw client that supplies
    /// an explicit, bound-matching `project=` cannot forge
    /// `__tachi_project_explicit: false` alongside it to make the write look
    /// like a transport default. Before the fix, `mark_project_explicit_
    /// unless_already_defaulted` preserved this wire-supplied `false`,
    /// letting the caller's OWN explicit placement decision get silently
    /// second-guessed by the write-affinity domain-reroute gate (which only
    /// re-evaluates the "default" case) — routing the write into a
    /// completely different, never-validated project store. The marker must
    /// now always be recomputed as `true` whenever `project` is genuinely
    /// present, regardless of what a forged wire value claims.
    #[test]
    fn forged_default_marker_on_explicit_project_is_overwritten() {
        with_test_home(|tachi_home| {
            let sigil = tachi_home.join("projects/sigil/memory.db");
            std::fs::create_dir_all(sigil.parent().expect("sigil parent")).expect("sigil parent");
            std::fs::write(&sigil, b"sigil").expect("sigil db");

            let mut arguments = args(map_from(&[
                ("action", json!("save")),
                ("project", json!("sigil")),
                ("text", json!("attacker-forged marker")),
                // A raw client can send this key directly — it is hidden
                // from the *published* schema (`#[schemars(skip)]`) but
                // `#[serde(rename = ...)]` still deserializes it from any
                // wire JSON that includes it.
                (PROJECT_EXPLICIT_MARKER, json!(false)),
            ]));
            enforce_session_project(
                "tachi_memory",
                &mut arguments,
                "sigil",
                "HTTP direct-connect",
                EnforcementRole::Authoritative,
            )
            .expect("explicit project matching the bound identity is accepted");
            assert_eq!(
                arguments
                    .as_ref()
                    .and_then(|a| a.get(PROJECT_EXPLICIT_MARKER))
                    .and_then(|v| v.as_bool()),
                Some(true),
                "a genuinely-present project= must never be second-guessed by a \
                 forged wire marker — this must always come out `true`"
            );
        });
    }

    /// #1041 F2/B1 core regression: the stdio-proxy -> daemon HTTP double
    /// hop, with the B1 fix's revised division of labor. The proxy
    /// (`Preflight`) runs `enforce_session_project` first on a request where
    /// the client omitted `project=` — it must leave the arguments
    /// completely untouched (no injected project, no marker at all), since
    /// injecting anything here would recreate a wire-visible "this hop
    /// decided it's a default" signal for an attacker to spoof. Only the
    /// daemon's own HTTP `call_tool` (`Authoritative`) actually injects the
    /// default and stamps the marker, and it is the ONLY hop that ever does
    /// so for this request.
    #[test]
    fn marker_survives_double_enforcement_across_proxy_and_daemon_hops() {
        // #1041 round-3 fixture fix: the Authoritative hop resolves the
        // injected bound project through `resolve_named_project_db_identity`
        // internally — needs `with_test_home` + a seeded
        // `projects/sigil/memory.db` for the same reason as
        // `genuinely_explicit_project_is_stamped_explicit` above.
        with_test_home(|tachi_home| {
            let sigil = tachi_home.join("projects/sigil/memory.db");
            std::fs::create_dir_all(sigil.parent().expect("sigil parent")).expect("sigil parent");
            std::fs::write(&sigil, b"sigil").expect("sigil db");

            let mut arguments = args(map_from(&[
                ("action", json!("save")),
                ("text", json!("forwarded through the stdio proxy")),
            ]));
            // Hop 1: stdio proxy, client omitted project=. Preflight must be
            // a no-op here — nothing to forge downstream.
            enforce_session_project(
                "tachi_memory",
                &mut arguments,
                "sigil",
                "stdio proxy",
                EnforcementRole::Preflight,
            )
            .expect("hop 1: preflight passes through");
            let after_hop1 = arguments.as_ref().expect("arguments present");
            assert!(
                after_hop1.get("project").is_none(),
                "Preflight must not inject a default project"
            );
            assert!(
                after_hop1.get(PROJECT_EXPLICIT_MARKER).is_none(),
                "Preflight must not stamp any marker for an untouched default"
            );
            // Hop 2: the daemon's own HTTP call_tool, same bound project,
            // is the sole Authoritative hop — it sees `project` genuinely
            // absent (hop 1 didn't touch it) and injects the default itself.
            enforce_session_project(
                "tachi_memory",
                &mut arguments,
                "sigil",
                "HTTP direct-connect",
                EnforcementRole::Authoritative,
            )
            .expect("hop 2: inject default project");
            assert_eq!(
                arguments
                    .as_ref()
                    .and_then(|a| a.get(PROJECT_EXPLICIT_MARKER))
                    .and_then(|v| v.as_bool()),
                Some(false),
                "the sole Authoritative hop must mark its own injected default \
                 as NOT explicit"
            );
        });
    }

    #[test]
    fn bound_session_does_not_force_project_on_global_scope() {
        let mut arguments = args(map_from(&[
            ("action", json!("search")),
            ("scope", json!("global")),
            ("query", json!("x")),
        ]));
        // Authoritative (not Preflight): exercises the scope=global bypass
        // inside the injection branch itself, not just "Preflight never
        // injects anything anyway".
        enforce_session_project(
            "tachi_memory",
            &mut arguments,
            "sigil",
            "HTTP direct-connect",
            EnforcementRole::Authoritative,
        )
        .expect("global scope search");
        assert!(
            arguments.as_ref().and_then(|a| a.get("project")).is_none(),
            "global scope must not get bound project injected"
        );
    }

    #[test]
    fn unbound_session_rejects_explicit_project_write() {
        let arguments = args(map_from(&[
            ("action", json!("save")),
            ("project", json!("victim")),
            ("text", json!("pwn")),
        ]));
        let err = reject_unbound_cross_project_write(
            "tachi_memory",
            &arguments,
            None,
            "HTTP direct-connect",
        )
        .expect_err("C1 must reject unbound write");
        assert!(
            err.message.contains("not bound to a project")
                && err.message.contains("victim")
                && err.message.contains("X-Tachi-Project"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn unbound_session_allows_explicit_project_read() {
        let arguments = args(map_from(&[
            ("action", json!("search")),
            ("project", json!("wiki")),
            ("query", json!("x")),
        ]));
        reject_unbound_cross_project_write("tachi_memory", &arguments, None, "HTTP direct-connect")
            .expect("unbound cross-project read allowed");
    }

    #[test]
    fn bound_session_skips_c1_unbound_guard() {
        let arguments = args(map_from(&[
            ("action", json!("save")),
            ("project", json!("sigil")),
            ("text", json!("ok")),
        ]));
        // C1 only applies when unbound; bound path uses enforce_session_project.
        reject_unbound_cross_project_write(
            "tachi_memory",
            &arguments,
            Some("sigil"),
            "HTTP direct-connect",
        )
        .expect("bound session not subject to unbound C1");
    }

    #[test]
    fn legacy_search_memory_is_cross_project_readable() {
        let args = map_from(&[("project", json!("other")), ("query", json!("q"))]);
        assert!(explicit_project_can_cross_binding("search_memory", &args));
        assert!(!explicit_project_can_cross_binding(
            "save_memory",
            &map_from(&[("project", json!("other")), ("text", json!("t"))])
        ));
    }

    // ── #1251: recursive-dispatch depth gate ─────────────────────────────────

    #[test]
    fn enforce_dispatch_depth_allows_below_limit() {
        // depth 0 (leader), 1, 2 all under a limit of 3 → allowed.
        for depth in 0..MAX_DISPATCH_DEPTH {
            enforce_dispatch_depth(depth, MAX_DISPATCH_DEPTH).unwrap_or_else(|err| {
                panic!("depth {depth} < {MAX_DISPATCH_DEPTH} must pass: {err}")
            });
        }
    }

    #[test]
    fn enforce_dispatch_depth_fails_closed_at_and_beyond_limit() {
        for depth in [MAX_DISPATCH_DEPTH, MAX_DISPATCH_DEPTH + 1, u32::MAX] {
            let err = enforce_dispatch_depth(depth, MAX_DISPATCH_DEPTH)
                .expect_err("depth >= limit must be refused");
            assert!(
                err.contains("recursive dispatch depth limit reached")
                    && err.contains("MAX_DISPATCH_DEPTH"),
                "unexpected error for depth {depth}: {err}"
            );
        }
    }

    #[test]
    fn resolve_dispatch_depth_absent_marker_is_leader_zero() {
        assert_eq!(resolve_dispatch_depth(None, MAX_DISPATCH_DEPTH), 0);
    }

    #[test]
    fn resolve_dispatch_depth_honors_present_well_formed_value() {
        assert_eq!(resolve_dispatch_depth(Some("0"), MAX_DISPATCH_DEPTH), 0);
        assert_eq!(resolve_dispatch_depth(Some("2"), MAX_DISPATCH_DEPTH), 2);
        // whitespace-padded (as an env/header round-trip can produce) is trimmed
        assert_eq!(resolve_dispatch_depth(Some("  1 "), MAX_DISPATCH_DEPTH), 1);
    }

    #[test]
    fn resolve_dispatch_depth_malformed_saturates_to_limit_fail_closed() {
        // non-numeric, negative, blank, and overflowing all saturate to the
        // limit so `enforce_dispatch_depth` then refuses — never an implicit
        // "allow" from a parse error.
        for raw in ["abc", "-1", "", "   ", "99999999999999999999", "1.5", "0x2"] {
            let resolved = resolve_dispatch_depth(Some(raw), MAX_DISPATCH_DEPTH);
            assert_eq!(
                resolved, MAX_DISPATCH_DEPTH,
                "malformed depth {raw:?} must saturate to the limit"
            );
            enforce_dispatch_depth(resolved, MAX_DISPATCH_DEPTH)
                .expect_err("a saturated malformed depth must fail the gate closed");
        }
    }

    #[test]
    fn resolve_dispatch_depth_present_value_at_limit_still_fails_closed() {
        // A genuinely-present depth equal to the limit is honored as-is (not
        // treated as absent) and the gate refuses it — the recursion actually
        // stops at the boundary.
        let resolved = resolve_dispatch_depth(Some("3"), MAX_DISPATCH_DEPTH);
        assert_eq!(resolved, 3);
        enforce_dispatch_depth(resolved, MAX_DISPATCH_DEPTH)
            .expect_err("a caller already at the limit must not dispatch");
    }

    #[test]
    fn normalize_identity_trims_and_rejects_empty() {
        assert_eq!(
            normalize_identity_value("  sigil  ").as_deref(),
            Some("sigil")
        );
        assert_eq!(normalize_identity_value("   "), None);
        assert_eq!(normalize_identity_value(""), None);
    }
}

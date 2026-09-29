//! #1098: single typed action-effect authority.
//!
//! Before this module, "does this operation mutate state, and is it safe to
//! replay" was represented independently in four places: `shared_defs`'s
//! `NON_IDEMPOTENT_TOOL_NAMES` + `FACADE_MUTATING_ACTIONS` string tables,
//! `server_state::cache`'s `CACHEABLE_TOOLS` + `CACHE_INVALIDATING_TOOLS`
//! string tables, `server_handler`'s DLQ-admission call site, and
//! `server_methods::skill`'s retry gate. Proxy-qualified routes must not borrow
//! replay authority from a similarly named local facade: only the owning
//! remote server could provide that authority, and no such registry exists.
//!
//! Owner ruling (2026-07-17, tachi#1098, adjudicated via leader package,
//! option A): both clauses ratified —
//!
//! 1. **External proxy routes fail closed.** A `server__tool` wire name has no
//!    local replay authority even when its tail matches a safe local facade.
//! 2. **Only explicitly typed read-only routes may replay.**
//!    [`facade_action_effect`] exhaustively maps the action enum advertised by
//!    each audited facade. Cacheability is not replay authority: standalone
//!    replay-safe routes have their own explicit inventory. All other routes
//!    are refused by the DLQ gate.
//!    Conservative refusal only costs retry throughput; an aggressive allow
//!    can replay an unsafe mutation.
//!
//! Cache eligibility keeps its own whole-tool-name granularity (unchanged
//! from pre-#1098: a facade call invalidates the cache regardless of which
//! action it carries, exactly as `CACHE_INVALIDATING_TOOLS` always did) —
//! that's the "their own narrow policy" the issue's frozen model allows on
//! top of the shared metadata; the two membership lists in this module are
//! the single source both `server_state::cache` and the completeness tests
//! below read from, not a fourth independently-authored list.
//!
//! The cache-invalidating inventory is also explicit replay metadata: every
//! standalone entry is `Mutating`/`Unsafe`, while an unlisted route has no
//! replay authority at all. This keeps a new mutator from inheriting a legacy
//! allow merely because the route inventory was not updated yet.

use serde_json::Value;

// ─── Types (the frozen model) ────────────────────────────────────────────────

/// Whether a routed operation reads or mutates state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionEffect {
    ReadOnly,
    Mutating,
}

/// Whether replaying (auto-retrying via the DLQ) a failed call is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplaySafety {
    /// Provably side-effect-free (or read-only) — safe to auto-retry.
    Safe,
    /// Mutating and not yet individually proven safe to replay. Treated as
    /// blocked today (same as `Unsafe`) — kept distinct from `Unsafe` so an
    /// action later proven idempotent can be promoted to `Safe` with a single,
    /// auditable, individually-justified whitelist entry instead of being
    /// lumped in with the actions that are known to duplicate writes on replay.
    Conditional,
    /// Replaying this call can duplicate a write or otherwise re-apply an
    /// action that was not designed to be re-entrant. Never auto-retried.
    Unsafe,
}

/// Typed metadata for one routed operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ActionEffectMetadata {
    pub(crate) effect: ActionEffect,
    pub(crate) replay: ReplaySafety,
}

impl ActionEffectMetadata {
    const READ_ONLY_SAFE: Self = Self {
        effect: ActionEffect::ReadOnly,
        replay: ReplaySafety::Safe,
    };
    const MUTATING_CONDITIONAL: Self = Self {
        effect: ActionEffect::Mutating,
        replay: ReplaySafety::Conditional,
    };
    const MUTATING_UNSAFE: Self = Self {
        effect: ActionEffect::Mutating,
        replay: ReplaySafety::Unsafe,
    };

    /// Only an explicitly `Safe` action may be auto-retried through the DLQ.
    fn permits_dlq_replay(self) -> bool {
        matches!(self.replay, ReplaySafety::Safe)
    }
}

// ─── Cache policy: whole-tool-name granularity, unchanged from pre-#1098 ────

/// Tools whose results can be cached (read-only, no side effects). Ported
/// verbatim from the pre-#1098 `server_state::cache::CACHEABLE_TOOLS`.
pub(crate) const CACHEABLE_TOOLS: &[&str] = &[
    "section_build",
    "tachi_wiki_search",
    "list_memories",
    "memory_stats",
    "hub_discover",
    "hub_get",
    "hub_stats",
    "vc_list",
    "vc_resolve",
    "get_pipeline_status",
    // Facade tools (read-only)
    "tachi_search",
    "tachi_web_search",
    "tachi_browse",
    "tachi_component",
];

/// Tools that invalidate the cache (write operations). Ported verbatim from
/// the pre-#1098 `server_state::cache::CACHE_INVALIDATING_TOOLS`, including
/// its known gaps (`tachi_gh`/`tachi_event` are not in this list, so calling
/// either never invalidates the cache today) — preserved as-is; fixing that
/// gap is a separate, unadjudicated change, not part of #1098's scope.
pub(crate) const CACHE_INVALIDATING_TOOLS: &[&str] = &[
    "extract_facts",
    "ingest_event",
    "hub_register",
    "hub_quick_add",
    "hub_review",
    "hub_set_active_version",
    "hub_export_skills",
    "capture_session",
    "archive_memory",
    "compact_rollup",
    "compact_session_memory",
    "sync_memories",
    "vc_register",
    "vc_bind",
    "hub_feedback",
    // v2 sandbox retirement: "sandbox_set_rule"/"sandbox_set_policy" routes
    // were deleted (aliases expired at 1.10.0). Their writes now invalidate the
    // cache via "tachi_sandbox" below; stale entries would fail the #1098
    // live-route census gate.
    "tachi_init_project_db",
    // #1099: "handoff_leave"/"handoff_check" retired — the routes no longer
    // exist. "tachi_handoff" (below) stays, still mixed read/write via its
    // one surviving action (promote_issue).
    // Batch B: "post_card"/"update_card" retired from MCP.
    "tachi_unstick",
    "wiki_lint",
    "tachi_wiki_write",
    "tachi_wiki_ingest",
    // Facade tools (write / mixed)
    "tachi_memory",
    "tachi_domain_adapter",
    "tachi_handoff",
    "tachi_task",
    "tachi_wiki",
    "tachi_skill",
    "tachi_verify",
    "tachi_staff",
    "tachi_a2a",
    // #1679 delivery seam is mixed read/write (claim/ack/block/dismiss
    // mutate the delivery spine; get is read).
    "tachi_delivery",
    // #757 Cut3-S1: folded sandbox verb is mixed read/write (set_rule/
    // set_policy mutate) — invalidate like the legacy sandbox_set_* aliases
    // above, matching the whole-facade invalidation used for tachi_memory.
    "tachi_sandbox",
];

// ─── Standalone (non-facade) native/proxy routes ─────────────────────────────

/// Standalone tool names that are never safe to auto-replay after a failure.
/// The first nine entries are ported verbatim from the pre-#1098
/// `shared_defs::NON_IDEMPOTENT_TOOL_NAMES`.
///
/// `remember` / `extract_facts` / `ingest_event` remain named here as direct
/// hard denials. Other cache-invalidating standalone routes receive the same
/// `Mutating`/`Unsafe` metadata in [`dlq_replay_metadata`], so aliases and
/// future registration cannot fall through to an implicit safe replay.
const STANDALONE_UNSAFE_ROUTES: &[&str] = &[
    "save_memory",
    "tachi_save",
    "tachi_wiki_write",
    "vault_set",
    "vault_remove",
    "vault_init",
    "vault_setup_rotation",
    "vault_set_api_key_pool",
    "hub_call",
    "remember",
    "extract_facts",
    "ingest_event",
    // Both production entrypoints persist access telemetry.
    "search_memory",
    "tachi_search",
];

/// Standalone routes with explicit replay authority. This is intentionally
/// separate from `CACHEABLE_TOOLS`: a cached operation may still write access
/// telemetry (`search_memory`), and therefore may not be replay-safe.
const STANDALONE_REPLAY_SAFE_ROUTES: &[&str] = &[
    "section_build",
    "tachi_wiki_search",
    "list_memories",
    "memory_stats",
    "hub_discover",
    "hub_get",
    "hub_stats",
    "vc_list",
    "vc_resolve",
    "get_pipeline_status",
    "tachi_web_search",
    "tachi_browse",
];

// ─── Clause 2: gated facades, default-deny per action ───────────────────────

/// Explicit effect map for every action advertised by a schema-enumerated
/// facade. The live-router ratchet compares schema enums to this independent
/// map, so a new action cannot inherit whole-tool cacheability.
pub(crate) fn facade_action_effect(
    tool_name: &str,
    action: Option<&str>,
) -> Option<ActionEffectMetadata> {
    let action = action.map(str::trim).filter(|action| !action.is_empty());
    let (read_only, conditional, unsafe_actions): (&[&str], &[&str], &[&str]) = match tool_name {
        // Search writes access telemetry through
        // `handle_search_memory_with_access(..., true)` and is therefore not
        // replay-safe. The remaining classifications are preserved.
        "tachi_memory" => (
            &["get", "briefing", "alerts", "ask"],
            &[],
            &[
                "search",
                "save",
                "extract_facts",
                "checkpoint",
                "consolidate",
            ],
        ),
        "tachi_event" => (
            &["query", "metrics", "a2a"],
            &["label_eval", "context"],
            &["emit", "project", "promote"],
        ),
        "tachi_a2a" => (&["status"], &[], &["respond"]),
        // #1679 delivery seam: `get` is read-only; every mutating seam
        // action re-enters the spine (claim/ack lease, block, dismiss) and
        // is not safe to auto-replay after a failure.
        "tachi_delivery" => (
            &["get"],
            &[],
            &[
                "claim_ready_delivery",
                "ack_delivered",
                "reject_or_block",
                "resume_requester_operation",
                "dismiss",
            ],
        ),
        "tachi_wiki" => (&["search", "browse", "read"], &[], &["write"]),
        "tachi_task" => (
            &["status", "board", "brief"],
            &[],
            &[
                "complete",
                "intake",
                "adjudicate",
                "claim",
                "release",
                "heartbeat",
                "handoff",
            ],
        ),
        "tachi_tune" => (
            &["route_simulate", "recall_simulate"],
            &["route_proposals", "recall_proposals"],
            &[
                "route_review",
                "route_apply",
                "recall_review",
                "recall_apply",
            ],
        ),
        "tachi_gh" => (
            &[
                "repo_view",
                "issue_list",
                "issue_read",
                "pr_list",
                "pr_read",
                "pr_comments",
                "pr_status",
            ],
            &[
                "issue_freshness_scan",
                "current_truth_refresh",
                "pr_review_digest",
            ],
            &[
                "issue_create",
                "issue_comment",
                "issue_label",
                "pr_comment",
                "safe_merge",
                "ship",
                "link_pr",
                "pr_handoff",
                "release_note",
                "close_loop",
            ],
        ),
        "tachi_staff" => (&["status", "preflight"], &[], &["start", "cancel"]),
        "tachi_component" => (&["list", "show", "check", "plan"], &[], &[]),
        // Preserve the existing conservative treatment of these facades while
        // making the set exhaustive. Unknown actions receive no metadata.
        "tachi_skill" => (&["discover"], &[], &["run"]),
        "tachi_verify" => (
            &[],
            &[],
            &[
                "start", "record", "status", "board",
                // #1454 slice 2: server-executed checks are side-effecting
                // (process spawn + ledger write) and never replay-safe.
                "run",
            ],
        ),
        _ => return None,
    };

    let Some(action) = action else {
        return Some(ActionEffectMetadata::MUTATING_UNSAFE);
    };

    if read_only
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(action))
    {
        Some(ActionEffectMetadata::READ_ONLY_SAFE)
    } else if conditional
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(action))
    {
        Some(ActionEffectMetadata::MUTATING_CONDITIONAL)
    } else if unsafe_actions
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(action))
    {
        Some(ActionEffectMetadata::MUTATING_UNSAFE)
    } else {
        None
    }
}

// ─── Public authority entry points ───────────────────────────────────────────

/// Returns the explicit typed effect/replay classification used for DLQ
/// admission. `None` is deliberately not an implicit read: callers must deny
/// replay and surface the missing authority rather than preserving an
/// unclassified proxy or newly registered route as safe.
pub(crate) fn dlq_replay_metadata(
    tool_name: &str,
    arguments: Option<&serde_json::Map<String, Value>>,
) -> Option<ActionEffectMetadata> {
    // A remote server's tool may share a tail with a local facade without
    // sharing its implementation or effects. No per-remote-tool authority is
    // registered today, so every proxy-qualified wire name fails closed.
    if tool_name.contains("__") {
        return None;
    }

    let action = arguments
        .and_then(|args| args.get("action"))
        .and_then(Value::as_str);

    facade_action_effect(tool_name, action).or_else(|| {
        if STANDALONE_UNSAFE_ROUTES.contains(&tool_name)
            || CACHE_INVALIDATING_TOOLS.contains(&tool_name)
        {
            Some(ActionEffectMetadata::MUTATING_UNSAFE)
        } else if STANDALONE_REPLAY_SAFE_ROUTES.contains(&tool_name) {
            Some(ActionEffectMetadata::READ_ONLY_SAFE)
        } else {
            None
        }
    })
}

/// The sole DLQ allow condition: a route must have an explicit typed
/// read-only/safe classification. Unknown names, external aliases, and future
/// actions fail closed.
pub(crate) fn dlq_replay_is_explicitly_safe(
    tool_name: &str,
    arguments: Option<&serde_json::Map<String, Value>>,
) -> bool {
    dlq_replay_metadata(tool_name, arguments).is_some_and(ActionEffectMetadata::permits_dlq_replay)
}

/// Recovery guidance for a failed invocation, not a receipt of its effects.
///
/// An error can arrive after a write committed, so a failed invocation never
/// proves that nothing happened — not even for a route the policy classifies
/// read-only, because reads like `search_memory` persist access telemetry. The
/// guidance therefore:
///
/// - reports `actual_effects: "not_established"` for every class;
/// - authorizes replay ONLY for an explicitly read-only/safe policy, and even
///   then never promises the failure was transient;
/// - refuses to advertise retry safety or rollback for a mutating or unknown
///   route, pointing the caller at canonical readback instead.
///
/// It is derived from [`dlq_replay_metadata`], the same authority the DLQ gate
/// uses, so a proxy-qualified or unclassified route cannot borrow a local
/// facade's replay authority.
pub(crate) fn failed_invocation_guidance(
    tool_name: &str,
    arguments: Option<&serde_json::Map<String, Value>>,
) -> Value {
    let metadata = dlq_replay_metadata(tool_name, arguments);
    let replay_authorized = metadata.is_some_and(ActionEffectMetadata::permits_dlq_replay);
    let effect_policy = match metadata.map(|m| m.effect) {
        Some(ActionEffect::ReadOnly) => "read_only",
        Some(ActionEffect::Mutating) => "may_mutate",
        None => "unknown",
    };
    let replay_basis = match metadata {
        Some(_) if replay_authorized => "read_only_policy",
        Some(_) => "mutating_not_replay_safe",
        None => "no_local_effect_authority",
    };
    serde_json::json!({
        "version": 1,
        "authority": "server_action_effect_policy",
        "invocation": "failed",
        "effect_policy": effect_policy,
        "actual_effects": "not_established",
        "replay_authorization": {
            "authorized": replay_authorized,
            "basis": replay_basis,
            "covers_transient_retry": false,
        },
        "next_action": if replay_authorized {
            "resolve_error_before_retry"
        } else {
            "read_back_canonical_state_before_any_retry"
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn dlq_unsafe(tool_name: &str, action: Option<&str>) -> bool {
        let args = action.map(|a| {
            serde_json::Map::from_iter([("action".to_string(), Value::String(a.to_string()))])
        });
        !dlq_replay_is_explicitly_safe(tool_name, args.as_ref())
    }

    // ── Clause 1: external routes cannot borrow local authority ─────────────

    #[test]
    fn f1098_remote_prefixed_facade_mutation_is_no_longer_a_bypass() {
        // Pre-#1098: the deny predicate compared the FACADE tuple against
        // the raw name "remote__tachi_memory", which never matched
        // "tachi_memory" exactly, so this returned `false` (safe to replay) —
        // the exact bug the owner's adjudication comment named.
        for action in ["save", "gc", "claim", "release"] {
            assert!(
                dlq_unsafe("remote__tachi_memory", Some(action)),
                "remote__tachi_memory(action='{action}') must be unsafe to replay post-#1098"
            );
        }
        for action in ["emit", "project", "promote"] {
            assert!(
                dlq_unsafe("remote__tachi_event", Some(action)),
                "remote__tachi_event(action='{action}') must be unsafe to replay post-#1098"
            );
        }
        // A read-only-looking tail is still an external implementation with no
        // server-owned per-tool replay authority.
        assert!(dlq_unsafe("remote__tachi_memory", Some("search")));
        assert!(dlq_unsafe("remote__tachi_event", Some("metrics")));
    }

    #[test]
    fn f1098_proxy_prefixed_standalone_names_fail_closed() {
        assert!(dlq_unsafe("remote__save_memory", None));
        assert!(dlq_unsafe("remote__hub_call", None));
        assert!(dlq_unsafe("remote__search_memory", None));
    }

    /// codex review (PR #1213, checkpoint 4): `remote__remember` used to resolve
    /// to `remember`, which was absent from
    /// `STANDALONE_UNSAFE_ROUTES` despite being cache-invalidating
    /// (state-mutating) — `facade_action_effect` also doesn't recognize
    /// `remember` as a facade, so the lookup fell all the way through to
    /// `.unwrap_or(false)` and the proxied route was misclassified safe to
    /// auto-retry. Same defect, same fix, for `extract_facts` /
    /// `ingest_event`. Discrimination: red before this fix-round's
    /// `STANDALONE_UNSAFE_ROUTES` addition, green after.
    #[test]
    fn f1213_direct_route_fail_open_closed_for_remember_extract_facts_ingest_event() {
        for tool in ["remember", "extract_facts", "ingest_event"] {
            assert!(
                dlq_unsafe(tool, None),
                "{tool} must be unsafe to replay (cache-invalidating, no action gate)"
            );
            let prefixed = format!("remote__{tool}");
            assert!(
                dlq_unsafe(&prefixed, None),
                "{prefixed} must stay unsafe without remote replay authority"
            );
        }
    }

    // ── Clause 2: default-deny for state-changing facade actions ───────────

    #[test]
    fn f1098_unknown_action_on_a_known_gated_facade_defaults_unsafe() {
        assert!(dlq_unsafe("tachi_memory", Some("some_future_action")));
        assert!(dlq_unsafe("tachi_event", Some("some_future_action")));
        assert!(dlq_unsafe("tachi_memory", None));
    }

    #[test]
    fn f1098_unaudited_facades_default_every_action_to_unsafe() {
        for tool in [
            "tachi_skill",
            "tachi_verify",
            "tachi_domain_adapter",
            "tachi_handoff",
            "tachi_orchestrator",
            "tachi_sandbox",
        ] {
            assert!(
                dlq_unsafe(tool, Some("anything")),
                "{tool} must default every action to unsafe-to-replay"
            );
            assert!(dlq_unsafe(tool, None));
        }
    }

    #[test]
    fn f1098_a_tool_outside_the_known_universe_fails_closed() {
        // A proxy or dynamically registered route without typed metadata is
        // never treated as a read merely because it is absent from old lists.
        assert!(dlq_unsafe("some_other_mcp_servers_tool", Some("anything")));
        assert!(dlq_unsafe("some_other_mcp_servers_tool", None));
    }

    // ── Behavior-freeze: previously-classified read-only actions stay safe ─

    #[test]
    fn f1098_previously_classified_read_only_actions_are_preserved() {
        for (tool, action) in [
            ("tachi_memory", "get"),
            ("tachi_memory", "briefing"),
            ("tachi_event", "query"),
            ("tachi_event", "metrics"),
            ("tachi_wiki", "search"),
            ("tachi_wiki", "read"),
            ("tachi_task", "status"),
            ("tachi_task", "board"),
            ("tachi_gh", "issue_read"),
            ("tachi_gh", "pr_status"),
            ("tachi_staff", "status"),
            ("tachi_component", "list"),
        ] {
            assert!(
                !dlq_unsafe(tool, Some(action)),
                "{tool}(action='{action}') must remain safe to replay"
            );
        }
    }

    #[test]
    fn f1098_previously_flagged_mutating_actions_stay_unsafe() {
        for (tool, action) in [
            ("tachi_memory", "search"),
            ("tachi_memory", "save"),
            ("tachi_event", "emit"),
            ("tachi_wiki", "write"),
            ("tachi_task", "complete"),
            ("tachi_staff", "start"),
            ("tachi_staff", "cancel"),
        ] {
            assert!(
                dlq_unsafe(tool, Some(action)),
                "{tool}(action='{action}') must remain unsafe to replay"
            );
        }
    }

    /// #1454 slice 2: `tachi_verify(action='run')` is a server-executed,
    /// side-effecting check — a replayed DLQ retry would spawn a second
    /// process run and double-write the ledger. It must never be classified
    /// replay-safe, and the whole-facade cache-invalidating membership must
    /// stay (the cache is invalidated regardless of which action runs).
    #[test]
    fn f1454_verify_run_action_is_never_replay_safe() {
        assert!(dlq_unsafe("tachi_verify", Some("run")));
        assert_eq!(
            dlq_replay_metadata(
                "tachi_verify",
                Some(&serde_json::Map::from_iter([(
                    "action".to_string(),
                    Value::String("run".to_string())
                )]))
            ),
            Some(ActionEffectMetadata::MUTATING_UNSAFE)
        );
        assert!(
            CACHE_INVALIDATING_TOOLS.contains(&"tachi_verify"),
            "tachi_verify must stay cache-invalidating (run/record/start all mutate the ledger)"
        );
    }

    #[test]
    fn retired_task_actions_are_unclassified() {
        for action in tachi_params::TACHI_TASK_RETIRED_ACTIONS {
            assert_eq!(
                facade_action_effect("tachi_task", Some(action)),
                None,
                "retired task action {action} must not have effect metadata"
            );
            assert!(
                dlq_unsafe("tachi_task", Some(action)),
                "retired task action {action} must fail closed for replay"
            );
        }
    }

    #[test]
    fn retired_memory_actions_are_unclassified() {
        for action in [
            "progress",
            "readiness",
            "delete",
            "gc",
            "doctor_scan",
            "ingest",
            "ingest_source",
            "pattern_feedback",
        ] {
            assert_eq!(facade_action_effect("tachi_memory", Some(action)), None);
        }
    }

    #[test]
    fn f1098_standalone_unsafe_routes_unchanged() {
        for name in STANDALONE_UNSAFE_ROUTES {
            assert!(
                dlq_unsafe(name, None),
                "{name} must remain unsafe to replay"
            );
        }
    }

    // ── Dynamic completeness: every typed facade action classifies ─────────

    fn assert_all_classified(tool: &str, actions: &[&str]) {
        for action in actions {
            assert!(
                facade_action_effect(tool, Some(action)).is_some(),
                "no #1098 classification for {tool}(action='{action}')"
            );
        }
    }

    /// codex review (PR #1213, checkpoint 3): the three inventories this test
    /// enumerated used to be handwritten local mirrors of the private
    /// `tachi_params::facade::{tachi_event_action_schema, tachi_wiki_action_schema}`
    /// / `orchestration::tachi_staff_action_schema` string literals, with no
    /// shared source to catch drift between the schema and this test. Those
    /// schema functions — plus `tachi_skill_action_schema` — now
    /// read from `tachi_params::facade::action_inventory` pub consts that this
    /// test also imports (`tachi_params::TACHI_EVENT_ACTIONS` etc.): one
    /// source, not a fourth independently-authored list.
    #[test]
    fn f1098_every_typed_facade_action_has_effect_metadata() {
        assert_all_classified("tachi_memory", tachi_params::TACHI_MEMORY_ACTIONS);
        assert_all_classified("tachi_tune", tachi_params::TACHI_TUNE_ACTIONS);
        assert_all_classified("tachi_gh", tachi_params::TACHI_GH_ACTIONS);
        let task_actions = tachi_params::TachiTaskAction::primary_wire_strings();
        assert_all_classified("tachi_task", &task_actions);
        assert_all_classified("tachi_event", tachi_params::TACHI_EVENT_ACTIONS);
        assert_all_classified("tachi_a2a", tachi_params::TACHI_A2A_ACTIONS);
        assert_all_classified("tachi_delivery", tachi_params::TACHI_DELIVERY_ACTIONS);
        assert_all_classified("tachi_wiki", tachi_params::TACHI_WIKI_ACTIONS);
        assert_all_classified("tachi_staff", tachi_params::TACHI_STAFF_ACTIONS);
        // codex checkpoint 3: "Typed TachiVerifyAction::ALL exists in
        // crates/tachi-params but is ignored." These four are in the
        // unaudited/always-Mutating+Unsafe bucket (facade_action_effect's
        // empty-whitelist arm), so this doesn't change their classification —
        // it proves the enumeration walks the REAL typed action universe for
        // them too, instead of never touching real inventories that exist.
        assert_all_classified("tachi_skill", tachi_params::TACHI_SKILL_ACTIONS);
        let verify_actions = tachi_params::TachiVerifyAction::all_wire_strings();
        assert_all_classified("tachi_verify", &verify_actions);
    }

    /// Every cache-invalidating standalone route is now typed as
    /// `Mutating`/`Unsafe`; no pending-adjudication allowlist may preserve an
    /// automatic-replay hole for a newly registered mutator.
    #[test]
    fn f1098_every_cache_invalidating_standalone_route_is_triaged_for_replay_safety() {
        for name in CACHE_INVALIDATING_TOOLS {
            assert_eq!(
                dlq_replay_metadata(name, None),
                Some(ActionEffectMetadata::MUTATING_UNSAFE),
                "'{name}' invalidates the cache and must be explicitly unsafe to replay"
            );
        }
    }

    /// #1098: `ActionEffect::ReadOnly` must only ever pair with
    /// `ReplaySafety::Safe` — the type only exposes one constructor for that
    /// combination (`READ_ONLY_SAFE`), but this pins the invariant so a
    /// future edit that adds a second `ReadOnly`-effect constant can't
    /// silently pair it with `Conditional`/`Unsafe`.
    #[test]
    fn f1098_read_only_effect_always_pairs_with_safe_replay() {
        assert_eq!(
            ActionEffectMetadata::READ_ONLY_SAFE.replay,
            ReplaySafety::Safe
        );
        for tool in [
            "tachi_memory",
            "tachi_event",
            "tachi_wiki",
            "tachi_task",
            "tachi_gh",
            "tachi_staff",
        ] {
            for action in ["search", "status", "read", "query"] {
                if let Some(meta) = facade_action_effect(tool, Some(action)) {
                    if matches!(meta.effect, ActionEffect::ReadOnly) {
                        assert_eq!(meta.replay, ReplaySafety::Safe);
                    }
                }
            }
        }
    }

    // ── Cache policy: single-sourced, membership unchanged ──────────────────

    #[test]
    fn f1098_cache_lists_have_no_duplicate_or_overlapping_entries() {
        let cacheable: BTreeSet<&str> = CACHEABLE_TOOLS.iter().copied().collect();
        let invalidating: BTreeSet<&str> = CACHE_INVALIDATING_TOOLS.iter().copied().collect();
        assert_eq!(
            cacheable.len(),
            CACHEABLE_TOOLS.len(),
            "CACHEABLE_TOOLS has a duplicate"
        );
        assert_eq!(
            invalidating.len(),
            CACHE_INVALIDATING_TOOLS.len(),
            "CACHE_INVALIDATING_TOOLS has a duplicate"
        );
        assert!(
            cacheable.is_disjoint(&invalidating),
            "a tool name must not be both cacheable and cache-invalidating"
        );
    }
}

#[cfg(test)]
mod issue_1825_cancel_effect_tests {
    use super::*;

    #[test]
    fn tachi_staff_cancel_schema_and_effect_contract() {
        let metadata = facade_action_effect("tachi_staff", Some("cancel"))
            .expect("cancel has explicit action-effect metadata");
        assert_eq!(metadata.effect, ActionEffect::Mutating);
        assert_eq!(metadata.replay, ReplaySafety::Unsafe);
    }
}

#[cfg(test)]
mod failed_invocation_guidance_tests {
    use super::*;

    fn args(action: &str) -> serde_json::Map<String, Value> {
        serde_json::Map::from_iter([("action".to_string(), Value::String(action.to_string()))])
    }

    /// Read policy: replay is authorized (the route is side-effect-free apart
    /// from telemetry), but the failure is never claimed to be transient and
    /// the guidance never claims nothing happened.
    #[test]
    fn read_only_policy_reports_safe_replay_without_claiming_transience_or_no_effects() {
        let guidance = failed_invocation_guidance("tachi_memory", Some(&args("get")));
        assert_eq!(
            guidance["effect_policy"],
            Value::String("read_only".to_string())
        );
        assert_eq!(
            guidance["actual_effects"],
            Value::String("not_established".to_string())
        );
        assert_eq!(
            guidance["replay_authorization"]["authorized"],
            Value::Bool(true)
        );
        assert_eq!(
            guidance["replay_authorization"]["basis"],
            Value::String("read_only_policy".to_string())
        );
        assert_eq!(
            guidance["replay_authorization"]["covers_transient_retry"],
            Value::Bool(false)
        );
    }

    /// A mutating route must never advertise retry safety or rollback.
    #[test]
    fn mutating_policy_refuses_retry_safety_and_rollback() {
        let guidance = failed_invocation_guidance("tachi_memory", Some(&args("save")));
        assert_eq!(
            guidance["effect_policy"],
            Value::String("may_mutate".to_string())
        );
        assert_eq!(
            guidance["replay_authorization"]["authorized"],
            Value::Bool(false)
        );
        assert_eq!(
            guidance["replay_authorization"]["basis"],
            Value::String("mutating_not_replay_safe".to_string())
        );
        let serialized = guidance.to_string();
        assert!(
            !serialized.contains("retry_safe"),
            "mutating guidance must not advertise retry safety: {serialized}"
        );
        assert!(
            !serialized.contains("rollback"),
            "mutating guidance must not claim rollback: {serialized}"
        );
    }

    /// A conditional (mutating, unproven) route is treated as unsafe too.
    #[test]
    fn conditional_policy_refuses_retry_safety() {
        let guidance = failed_invocation_guidance("tachi_gh", Some(&args("issue_freshness_scan")));
        assert_eq!(
            guidance["effect_policy"],
            Value::String("may_mutate".to_string())
        );
        assert_eq!(
            guidance["replay_authorization"]["authorized"],
            Value::Bool(false)
        );
    }

    /// An unclassified route has no local effect authority at all.
    #[test]
    fn unknown_route_reports_unknown_policy_and_no_authority() {
        let guidance = failed_invocation_guidance("some_other_servers_tool", None);
        assert_eq!(
            guidance["effect_policy"],
            Value::String("unknown".to_string())
        );
        assert_eq!(
            guidance["replay_authorization"]["authorized"],
            Value::Bool(false)
        );
        assert_eq!(
            guidance["replay_authorization"]["basis"],
            Value::String("no_local_effect_authority".to_string())
        );
        assert_eq!(
            guidance["actual_effects"],
            Value::String("not_established".to_string())
        );
    }

    /// A proxy-qualified route must not borrow the tail's local read authority
    /// even when that tail is a known read-only facade action.
    #[test]
    fn proxy_route_cannot_borrow_local_read_authority() {
        let guidance = failed_invocation_guidance("remote__tachi_memory", Some(&args("get")));
        assert_eq!(
            guidance["effect_policy"],
            Value::String("unknown".to_string())
        );
        assert_eq!(
            guidance["replay_authorization"]["authorized"],
            Value::Bool(false)
        );
    }

    /// Guidance is a recovery fact, never an effect receipt / adjudication.
    #[test]
    fn guidance_never_claims_established_effects() {
        for (tool, action) in [
            ("tachi_memory", Some("get")),
            ("tachi_memory", Some("save")),
            ("totally_unknown", None),
        ] {
            let args = action.map(args);
            let guidance = failed_invocation_guidance(tool, args.as_ref());
            assert_eq!(
                guidance["actual_effects"],
                Value::String("not_established".to_string()),
                "{tool}({action:?}) must not mint an effect receipt"
            );
        }
    }
}

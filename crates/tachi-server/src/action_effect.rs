//! #1098: single typed action-effect authority.
//!
//! Before this module, "does this operation mutate state, and is it safe to
//! replay" was represented independently in four places: `shared_defs`'s
//! `NON_IDEMPOTENT_TOOL_NAMES` + `FACADE_MUTATING_ACTIONS` string tables,
//! `server_state::cache`'s `CACHEABLE_TOOLS` + `CACHE_INVALIDATING_TOOLS`
//! string tables, `server_handler`'s DLQ-admission call site, and
//! `server_methods::skill`'s retry gate. Exact-name facade matching also let
//! a canonicalized/remote-prefixed route (`remote__tachi_memory`) bypass
//! mutation classification entirely, because the facade check compared the
//! *raw* incoming name instead of its resolved tail.
//!
//! Owner ruling (2026-07-17, tachi#1098, adjudicated via leader package,
//! option A): both clauses ratified —
//!
//! 1. **Canonicalize routes BEFORE any policy judgment.** [`canonical_route_name`]
//!    is the one place a proxy-prefixed name is resolved to its bare tail;
//!    every classification lookup in this module receives the canonical name,
//!    never the raw wire name. A remote/relay route can no longer bypass
//!    mutation classification by shape alone.
//! 2. **Every state-changing facade action defaults to `Mutating`/`Unsafe`.**
//!    [`facade_action_effect`] is whitelist-shaped: it enumerates the actions
//!    that are provably read-only (cited against their param-doc evidence
//!    inline below) and everything else — including an action this module
//!    has never seen — falls through to the conservative default. Conservative
//!    misclassification only costs cache-hit-rate or retry throughput;
//!    aggressive misclassification replays an unsafe mutation, which is the
//!    more expensive failure mode this ruling forecloses.
//!
//! Cache eligibility keeps its own whole-tool-name granularity (unchanged
//! from pre-#1098: a facade call invalidates the cache regardless of which
//! action it carries, exactly as `CACHE_INVALIDATING_TOOLS` always did) —
//! that's the "their own narrow policy" the issue's frozen model allows on
//! top of the shared metadata; the two membership lists in this module are
//! the single source both `server_state::cache` and the completeness tests
//! below read from, not a fourth independently-authored list.
//!
//! PR #1213 fix round (codex cross-vendor review, 2026-07-17): closed a
//! direct-route fail-open for `remember`/`extract_facts`/`ingest_event`
//! (checkpoint 4, [`STANDALONE_UNSAFE_ROUTES`]'s doc comment), and replaced
//! the tautological "does every known facade action classify" completeness
//! test (checkpoint 3 — trivially always true by clause 2's default-deny)
//! with a direct-tool-route completeness gate
//! (`f1098_every_cache_invalidating_standalone_route_is_triaged_for_replay_safety`)
//! that CAN fail: every standalone entry in `CACHE_INVALIDATING_TOOLS` must
//! be explicitly triaged into either `STANDALONE_UNSAFE_ROUTES` or the
//! documented `KNOWN_UNADJUDICATED_STANDALONE_REPLAY_GAPS` allowlist. The
//! latter enumerates pre-existing (pre-#1098) standalone fail-open gaps that
//! were not part of the owner's 2026-07-17 adjudication and are therefore
//! flagged, not silently fixed, per the issue's behavior-freeze boundary.

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
    fn dlq_replay_unsafe(self) -> bool {
        !matches!(self.replay, ReplaySafety::Safe)
    }
}

// ─── Clause 1: canonicalize before judgment ──────────────────────────────────

/// Resolve a possibly proxy-prefixed wire name (`server__tool`) to its bare
/// tail. Every classification lookup below receives the output of this
/// function, never the raw incoming name — the fix for the remote-route
/// bypass the owner's adjudication comment identified
/// (`remote__tachi_memory(action='save')` previously matched neither the
/// native-tool-name table, keyed on the tail, nor the facade-name check,
/// which compared the raw name — so it fell through both and was classified
/// safe to replay).
///
/// Same fallback heuristic `server_handler::split_proxy_tool_name` uses when
/// no registered-server list is available: this is a free function with no
/// access to `self.tool_discovery.proxy_tools`, so it applies the
/// context-free "last `__` wins" rule uniformly.
pub(crate) fn canonical_route_name(tool_name: &str) -> &str {
    tool_name.rsplit("__").next().unwrap_or(tool_name)
}

// ─── Cache policy: whole-tool-name granularity, unchanged from pre-#1098 ────

/// Tools whose results can be cached (read-only, no side effects). Ported
/// verbatim from the pre-#1098 `server_state::cache::CACHEABLE_TOOLS`.
pub(crate) const CACHEABLE_TOOLS: &[&str] = &[
    "section_build",
    "recommend_capability",
    "recommend_skill",
    "recommend_toolchain",
    "prepare_capability_bundle",
    "tachi_task_brief",
    "tachi_wiki_search",
    "search_memory",
    "find_similar_memory",
    "get_memory",
    "list_memories",
    "memory_stats",
    "hub_discover",
    "hub_get",
    "hub_stats",
    "vc_list",
    "vc_resolve",
    "get_pipeline_status",
    "wiki_search",
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
    "save_memory",
    "remember",
    "extract_facts",
    "ingest_event",
    "hub_register",
    "hub_quick_add",
    "hub_review",
    "hub_set_active_version",
    "hub_export_skills",
    "skill_evolve",
    "capture_session",
    "archive_memory",
    "compact_rollup",
    "compact_session_memory",
    "sync_memories",
    "vc_register",
    "vc_bind",
    "hub_feedback",
    "sandbox_set_rule",
    "sandbox_set_policy",
    "tachi_init_project_db",
    // #1099: "handoff_leave"/"handoff_check" retired — the routes no longer
    // exist. "tachi_handoff" (below) stays, still mixed read/write via its
    // one surviving action (promote_issue).
    "post_card",
    "update_card",
    "distill_trajectory",
    "tachi_unstick",
    "wiki_lint",
    "tachi_wiki_write",
    "tachi_wiki_ingest",
    // Facade tools (write / mixed)
    "tachi_save",
    "tachi_memory",
    "tachi_domain_adapter",
    "tachi_handoff",
    "tachi_complete",
    "tachi_orchestrator",
    "tachi_task",
    "tachi_wiki",
    "tachi_skill",
    "tachi_arena",
    "tachi_verify",
    "tachi_shell",
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
/// `remember` / `extract_facts` / `ingest_event` are a fix-round addition
/// (codex review, PR #1213, checkpoint 4): all three are cache-invalidating
/// (`CACHE_INVALIDATING_TOOLS` above already treats them as state-mutating)
/// but were absent here, so a canonicalized/proxied route whose tail
/// resolves to one of these three names (e.g. `remote__remember`) fell
/// through both `STANDALONE_UNSAFE_ROUTES` and every `facade_action_effect`
/// match arm to `dlq_mutation_is_unsafe`'s `.unwrap_or(false)` — misclassified
/// safe-to-replay, letting `retry_dispatch` auto-retry a mutation via
/// `proxy_call_internal`. A *native* call to any of the three never reaches
/// this table at all (`should_enqueue_dlq`'s `is_native_route` short-circuit
/// excludes it before `dlq_mutation_is_unsafe` is even consulted; see
/// `server_handler.rs`'s DLQ capture call site), so this fix only changes
/// behavior for the proxied/remote path, matching the owner's 2026-07-17
/// ruling's "native-route behavior remains unchanged" clause. This is the
/// same class of pre-existing standalone-route gap as
/// `KNOWN_UNADJUDICATED_STANDALONE_REPLAY_GAPS` below (neither list item was
/// named in `NON_IDEMPOTENT_TOOL_NAMES` pre-#1098) — these three are fixed
/// because codex's checkpoint 4 named them with a live execution trace; the
/// rest are flagged, not fixed, pending separate adjudication.
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
];

// ─── Clause 2: gated facades, default-deny per action ───────────────────────

/// Facades DLQ/retry classification is aware of at all. The six that carry an
/// explicit read-only whitelist below (`tachi_memory` … `tachi_shell`) mirror
/// the pre-#1098 `FACADE_MUTATING_ACTIONS` facade tuple exactly — those are
/// the surfaces this module has actually audited action-by-action. The
/// remaining eight (`tachi_skill` … `tachi_complete`) are facades that also
/// carry a generic `action` argument but have not been individually audited;
/// per the owner's ruling every one of their actions defaults to
/// `Mutating`/`Unsafe` unconditionally (the empty-whitelist arm below) rather
/// than silently falling through unclassified.
fn facade_action_effect(
    canonical_tool: &str,
    action: Option<&str>,
) -> Option<ActionEffectMetadata> {
    let action = action.map(str::trim).filter(|a| !a.is_empty());

    let (read_only, conditional): (&[&str], &[&str]) = match canonical_tool {
        // search/get/briefing/alerts/ask: pure reads or Q&A-over-evidence per
        // TachiMemoryParams action doc. recall_simulate: "report recall@k/MRR
        // without mutating access counters" (explicit). readiness: "health +
        // tool visibility". doctor_scan: "read-only scan of memory.db roots"
        // (explicit, matches the pre-existing doctor_scan cache carve-out).
        //
        // sticky_check is deliberately NOT here even though it is Observe-tier
        // for authorization purposes: its own param doc says the *default*
        // (include_read=false) call "claims" unread stickies, i.e. consumes
        // the read-once note — the owner's adjudication comment named this
        // exact mismatch. claim/release/gc/sticky_leave were the other
        // actions the owner's comment named as missing from the old
        // FACADE_MUTATING_ACTIONS check; all four fall to the default below.
        "tachi_memory" => (
            &[
                "search",
                "get",
                "briefing",
                "alerts",
                "ask",
                "recall_simulate",
                "readiness",
                "doctor_scan",
            ],
            // recall_proposals ("generate/list evidence-backed RecallConfig
            // proposals") and progress (its own field doc implies recording
            // an event name, e.g. step_done/failed) are not clearly
            // side-effect-free — Conditional, not whitelisted Safe.
            &["recall_proposals", "progress"],
        ),
        // query/metrics/a2a are explicitly documented read-only ("metrics
        // returns read-only continuity metrics"; "a2a returns the read-only
        // evidence/open-thread bundle without feedback writes"). `context` is
        // deliberately excluded despite reading like a query: its own action
        // doc says it "records seen feedback" — a real write hidden behind a
        // read-shaped name, the same trap as tachi_memory's sticky_check.
        // emit/project/promote are the three actions the owner's adjudication
        // comment named directly as missing from the old classification.
        "tachi_event" => (&["query", "metrics", "a2a"], &["label_eval", "context"]),
        // write is the only mutating action; search/browse/read are plain reads.
        "tachi_wiki" => (&["search", "browse", "read"], &[]),
        // status/board/wait/briefing/doc_index/cycle_status/cycle_plan/
        // profiles/profile are read/report actions per TachiTaskParams's
        // action doc ("cycle_status/cycle_plan are read-only lifecycle
        // models"). refine_issues is explicitly "read-only, proposal-only …
        // it never closes/reopens/edits/writes back". route_simulate mirrors
        // tachi_memory's recall_simulate naming convention (simulate = no
        // persistence).
        "tachi_task" => (
            &[
                "status",
                "board",
                "wait",
                "briefing",
                "doc_index",
                "cycle_status",
                "cycle_plan",
                "refine_issues",
                "profiles",
                "profile",
                "route_simulate",
            ],
            // recommend/proposals "manage routing" per the action doc with no
            // explicit no-write disclaimer (unlike route_simulate/
            // refine_issues) — Conditional pending individual audit.
            &["recommend", "proposals"],
        ),
        // repo_view/issue_list/issue_read/pr_list/pr_read/pr_comments/
        // pr_status are named-and-shaped as pure GitHub reads.
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
            // pr_review_digest's own param doc: "Write pr_review_digest
            // artifacts under .tachi/reviews. Defaults to true." —
            // filesystem write, not whitelisted Safe. issue_freshness_scan
            // produces a scan report with no read-only disclaimer.
            &["issue_freshness_scan", "pr_review_digest"],
        ),
        // status is the only clear read; the other six are workflow stages
        // that advance state ("Required Tachi shell workflow stage").
        "tachi_shell" => (&["status"], &[]),
        // Facades with a generic `action` concept that have not been
        // individually audited action-by-action. Every action on them
        // defaults to Mutating+Unsafe (empty whitelist).
        "tachi_skill"
        | "tachi_verify"
        | "tachi_domain_adapter"
        | "tachi_handoff"
        | "tachi_orchestrator"
        | "tachi_arena"
        | "tachi_sandbox"
        | "tachi_complete" => (&[], &[]),
        _ => return None,
    };

    let Some(action) = action else {
        // A gated facade with no action at all is state-changing by default
        // (clause 2) — never treat a missing action as read-only.
        return Some(ActionEffectMetadata::MUTATING_UNSAFE);
    };

    if read_only.iter().any(|a| a.eq_ignore_ascii_case(action)) {
        Some(ActionEffectMetadata::READ_ONLY_SAFE)
    } else if conditional.iter().any(|a| a.eq_ignore_ascii_case(action)) {
        Some(ActionEffectMetadata::MUTATING_CONDITIONAL)
    } else {
        Some(ActionEffectMetadata::MUTATING_UNSAFE)
    }
}

// ─── Public authority entry points ───────────────────────────────────────────

/// Returns true when replaying `tool_name` through the DLQ could duplicate
/// writes. Canonicalizes `tool_name` (clause 1) before consulting either the
/// standalone-route table or the gated-facade table (clause 2's default-deny
/// applies only within a facade this module recognizes — a tool name outside
/// both tables is unchanged from pre-#1098 and returns `false`, i.e. not
/// flagged unsafe, matching legacy behavior for arbitrary external routes).
pub(crate) fn dlq_mutation_is_unsafe(
    tool_name: &str,
    arguments: Option<&serde_json::Map<String, Value>>,
) -> bool {
    let canonical = canonical_route_name(tool_name);

    if STANDALONE_UNSAFE_ROUTES.contains(&canonical) {
        return true;
    }

    let action = arguments
        .and_then(|args| args.get("action"))
        .and_then(Value::as_str);

    facade_action_effect(canonical, action)
        .map(ActionEffectMetadata::dlq_replay_unsafe)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn dlq_unsafe(tool_name: &str, action: Option<&str>) -> bool {
        let args = action.map(|a| {
            serde_json::Map::from_iter([("action".to_string(), Value::String(a.to_string()))])
        });
        dlq_mutation_is_unsafe(tool_name, args.as_ref())
    }

    // ── Clause 1: canonicalization closes the remote-route bypass ──────────

    #[test]
    fn f1098_remote_prefixed_facade_mutation_is_no_longer_a_bypass() {
        // Pre-#1098: dlq_mutation_is_unsafe compared the FACADE tuple against
        // the raw name "remote__tachi_memory", which never matched
        // "tachi_memory" exactly, so this returned `false` (safe to replay) —
        // the exact bug the owner's adjudication comment named.
        for action in [
            "save",
            "gc",
            "claim",
            "release",
            "sticky_leave",
            "sticky_check",
        ] {
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
        // A read-only action through the same remote prefix stays safe.
        assert!(!dlq_unsafe("remote__tachi_memory", Some("search")));
        assert!(!dlq_unsafe("remote__tachi_event", Some("metrics")));
    }

    #[test]
    fn f1098_canonicalization_also_covers_native_standalone_names() {
        // Same tail heuristic must still classify a proxy-prefixed standalone
        // tool name, matching pre-#1098 NON_IDEMPOTENT_TOOL_NAMES behavior.
        assert!(dlq_unsafe("remote__save_memory", None));
        assert!(dlq_unsafe("remote__hub_call", None));
        assert!(!dlq_unsafe("remote__search_memory", None));
    }

    /// codex review (PR #1213, checkpoint 4): `remote__remember` used to
    /// canonicalize to `remember`, which was absent from
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
                "{prefixed} must canonicalize to {tool} and stay unsafe to replay"
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
            "tachi_arena",
            "tachi_sandbox",
            "tachi_complete",
        ] {
            assert!(
                dlq_unsafe(tool, Some("anything")),
                "{tool} must default every action to unsafe-to-replay"
            );
            assert!(dlq_unsafe(tool, None));
        }
    }

    #[test]
    fn f1098_a_tool_outside_the_known_universe_is_unchanged() {
        // Not a standalone route, not a recognized facade — legacy default
        // (`false`) is preserved; #1098 does not widen classification to
        // arbitrary external/unknown tool names.
        assert!(!dlq_unsafe("some_other_mcp_servers_tool", Some("anything")));
        assert!(!dlq_unsafe("some_other_mcp_servers_tool", None));
    }

    // ── Behavior-freeze: previously-classified read-only actions stay safe ─

    #[test]
    fn f1098_previously_classified_read_only_actions_are_preserved() {
        for (tool, action) in [
            ("tachi_memory", "search"),
            ("tachi_memory", "get"),
            ("tachi_memory", "briefing"),
            ("tachi_memory", "doctor_scan"),
            ("tachi_event", "query"),
            ("tachi_event", "metrics"),
            ("tachi_wiki", "search"),
            ("tachi_wiki", "read"),
            ("tachi_task", "status"),
            ("tachi_task", "board"),
            ("tachi_gh", "issue_read"),
            ("tachi_gh", "pr_status"),
            ("tachi_shell", "status"),
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
            ("tachi_memory", "save"),
            ("tachi_memory", "delete"),
            ("tachi_event", "emit"),
            ("tachi_wiki", "write"),
            ("tachi_task", "dispatch"),
            ("tachi_task", "complete"),
            ("tachi_task", "merge"),
            ("tachi_shell", "dispatch"),
        ] {
            assert!(
                dlq_unsafe(tool, Some(action)),
                "{tool}(action='{action}') must remain unsafe to replay"
            );
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
    /// / `orchestration::tachi_shell_action_schema` string literals, with no
    /// shared source to catch drift between the schema and this test. Those
    /// three schema functions — plus `tachi_skill_action_schema`,
    /// `tachi_arena_action_schema`, `tachi_orchestrator_action_schema` — now
    /// read from `tachi_params::facade::action_inventory` pub consts that this
    /// test also imports (`tachi_params::TACHI_EVENT_ACTIONS` etc.): one
    /// source, not a fourth independently-authored list.
    #[test]
    fn f1098_every_typed_facade_action_has_effect_metadata() {
        assert_all_classified("tachi_memory", tachi_params::TACHI_MEMORY_ACTIONS);
        assert_all_classified("tachi_gh", tachi_params::TACHI_GH_ACTIONS);
        let task_actions = tachi_params::TachiTaskAction::primary_wire_strings();
        assert_all_classified("tachi_task", &task_actions);
        assert_all_classified("tachi_event", tachi_params::TACHI_EVENT_ACTIONS);
        assert_all_classified("tachi_wiki", tachi_params::TACHI_WIKI_ACTIONS);
        assert_all_classified("tachi_shell", tachi_params::TACHI_SHELL_ACTIONS);
        // codex checkpoint 3: "Typed TachiVerifyAction::ALL exists in
        // crates/tachi-params but is ignored." These four are in the
        // unaudited/always-Mutating+Unsafe bucket (facade_action_effect's
        // empty-whitelist arm), so this doesn't change their classification —
        // it proves the enumeration walks the REAL typed action universe for
        // them too, instead of never touching real inventories that exist.
        assert_all_classified("tachi_skill", tachi_params::TACHI_SKILL_ACTIONS);
        assert_all_classified("tachi_arena", tachi_params::TACHI_ARENA_ACTIONS);
        assert_all_classified(
            "tachi_orchestrator",
            tachi_params::TACHI_ORCHESTRATOR_ACTIONS,
        );
        let verify_actions = tachi_params::TachiVerifyAction::all_wire_strings();
        assert_all_classified("tachi_verify", &verify_actions);
    }

    /// #1098 direct-route completeness (acceptance: "every ... direct tool
    /// route ... has effect/replay metadata or fails a completeness test").
    /// Facade actions are default-deny by construction —
    /// `facade_action_effect`'s final arm means `is_some()` is trivially
    /// always true for a known facade and cannot, by itself, tell a reviewed
    /// classification from an unclassified one; that structural guarantee
    /// (not a test) is what makes facade-action coverage total (codex review,
    /// PR #1213, checkpoint 3).
    ///
    /// A *standalone* route has no such default: a canonical name that is
    /// neither in `STANDALONE_UNSAFE_ROUTES` nor a recognized facade silently
    /// falls through `facade_action_effect`'s `_ => None` arm to
    /// `dlq_mutation_is_unsafe`'s `.unwrap_or(false)` — legacy "outside the
    /// known universe" behavior
    /// (`f1098_a_tool_outside_the_known_universe_is_unchanged`) — even when
    /// the tool provably mutates state. That is exactly the checkpoint-4 bug
    /// this fix round closed for `remember`/`extract_facts`/`ingest_event`.
    ///
    /// This test makes the rest of that class of gap structurally visible
    /// instead of silent: every standalone (non-facade) entry in
    /// `CACHE_INVALIDATING_TOOLS` — this module's own typed authority for
    /// "this route mutates state" — must land in EITHER
    /// `STANDALONE_UNSAFE_ROUTES` OR the explicit,
    /// individually-commented `KNOWN_UNADJUDICATED_STANDALONE_REPLAY_GAPS`
    /// allowlist below. A future tool added to `CACHE_INVALIDATING_TOOLS`
    /// that lands in neither fails this test instead of disappearing into
    /// the same silent fail-open path unnoticed.
    #[test]
    fn f1098_every_cache_invalidating_standalone_route_is_triaged_for_replay_safety() {
        const KNOWN_FACADES: &[&str] = &[
            "tachi_memory",
            "tachi_event",
            "tachi_wiki",
            "tachi_task",
            "tachi_gh",
            "tachi_shell",
            "tachi_skill",
            "tachi_verify",
            "tachi_domain_adapter",
            "tachi_handoff",
            "tachi_orchestrator",
            "tachi_arena",
            "tachi_sandbox",
            "tachi_complete",
        ];
        for name in CACHE_INVALIDATING_TOOLS {
            if KNOWN_FACADES.contains(name) {
                // Facade actions are triaged by `facade_action_effect`, not
                // by this whole-tool-name gate.
                continue;
            }
            assert!(
                STANDALONE_UNSAFE_ROUTES.contains(name)
                    || KNOWN_UNADJUDICATED_STANDALONE_REPLAY_GAPS.contains(name),
                "'{name}' invalidates the cache (mutates state per this module's own \
                 typed authority) but is neither in STANDALONE_UNSAFE_ROUTES nor \
                 documented as a pending-adjudication gap in \
                 KNOWN_UNADJUDICATED_STANDALONE_REPLAY_GAPS — triage it into one of \
                 the two instead of leaving it silently unclassified"
            );
        }
    }

    /// #1098 (PR #1213 fix round, codex checkpoint 4): standalone routes that
    /// `CACHE_INVALIDATING_TOOLS` already marks as state-mutating but that
    /// were ALSO already replay-classified `false` (safe) pre-#1098 — absent
    /// from the legacy `NON_IDEMPOTENT_TOOL_NAMES` this module's
    /// `STANDALONE_UNSAFE_ROUTES` ported verbatim. This fail-open gap
    /// pre-dates #1098; it is not the specific bypass the owner's
    /// 2026-07-17 adjudication comment named (that comment named
    /// *facade-action* gaps — tachi_memory's
    /// gc/claim/release/sticky_leave/sticky_check, tachi_event's
    /// emit/project/promote — all closed by clause 2's default-deny).
    /// Closing every entry here is a separate, unadjudicated behavior change
    /// (the same category `CACHE_INVALIDATING_TOOLS`'s own doc comment
    /// already carves out for the tachi_gh/tachi_event cache-invalidation
    /// gap as "not part of #1098's scope") — flagged here per the issue's "a
    /// mismatch discovered in the baseline is flagged for adjudication; do
    /// not silently normalize" boundary, not silently fixed by this fix
    /// round. `remember`/`extract_facts`/`ingest_event` were the three
    /// codex's checkpoint 4 named with a live execution trace and are fixed
    /// (removed from this list, added to `STANDALONE_UNSAFE_ROUTES`); the
    /// rest are flagged, not fixed.
    const KNOWN_UNADJUDICATED_STANDALONE_REPLAY_GAPS: &[&str] = &[
        "hub_register",
        "hub_quick_add",
        "hub_review",
        "hub_set_active_version",
        "hub_export_skills",
        "skill_evolve",
        "capture_session",
        "archive_memory",
        "compact_rollup",
        "compact_session_memory",
        "sync_memories",
        "vc_register",
        "vc_bind",
        "hub_feedback",
        "sandbox_set_rule",
        "sandbox_set_policy",
        "tachi_init_project_db",
        "post_card",
        "update_card",
        "distill_trajectory",
        "tachi_unstick",
        "wiki_lint",
        "tachi_wiki_ingest",
    ];

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
            "tachi_shell",
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

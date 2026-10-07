//! F0 facade action inventory (#913 / #495).
//!
//! Machine-checkable lists of MCP facade actions so action-count regressions
//! are visible. `tachi_task` primary actions and `tachi_verify` actions are
//! derived from their typed enums (`action_enums::TachiTaskAction::PRIMARY`,
//! `TachiVerifyAction::ALL`) — no duplicated `&[&str]` mirrors (#1085).
//! GitHub PR lifecycle actions live only on `tachi_gh` (canonical); they were
//! removed from `tachi_task` in #757.

/// GH PR lifecycle actions removed from `tachi_task` (#757). Canonical surface
/// is `tachi_gh`. Kept as a machine-checkable deny-list for schema/router tests.
pub const TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS: &[&str] =
    &["link_pr", "pr_status", "pr_handoff", "release_note"];

/// C1a of #1611/#1683, C1b-1 of #1712, and #1713 retired these primary
/// `tachi_task` actions without leaving them in the live Task inventory. Keep
/// a machine-checkable deny-list so schemas, docs, and active profile
/// discriminators cannot keep teaching them.
pub const TACHI_TASK_RETIRED_C1A_ACTIONS: &[&str] = &[
    "plan",
    "cycle_plan",
    "recommend",
    "refine_issues",
    "merge",
    "ux_matrix",
    "briefing",
    "doc_index",
    "cycle_status",
    "build_references",
    "close_loop",
];

/// C1c of #1687 retires model-facing profile/card inspection from
/// `tachi_task`. The local operator-only `tachi card` command remains the
/// diagnostics surface; evaluators and static dispatch admission remain
/// separate owners.
pub const TACHI_TASK_RETIRED_C1C_ACTIONS: &[&str] = &["profiles", "profile", "card"];

/// Complete deny-list for retired `tachi_task` action tokens across C1a/b/c.
pub const TACHI_TASK_RETIRED_ACTIONS: &[&str] = &[
    "plan",
    "cycle_plan",
    "recommend",
    "refine_issues",
    "merge",
    "ux_matrix",
    "briefing",
    "doc_index",
    "cycle_status",
    "build_references",
    "close_loop",
    "profiles",
    "profile",
    "card",
];

/// Canonical WorkClaim lifecycle actions exposed on `tachi_task` by #1253.
#[cfg(test)]
const TACHI_TASK_WORKCLAIM_ACTIONS: &[&str] = &["claim", "release", "heartbeat", "handoff"];

/// Canonical `tachi_gh` actions (includes lifecycle).
pub const TACHI_GH_ACTIONS: &[&str] = &[
    "repo_view",
    "issue_list",
    "issue_read",
    "issue_create",
    "issue_comment",
    "issue_label",
    "issue_freshness_scan",
    "current_truth_refresh",
    "pr_list",
    "pr_read",
    "pr_comments",
    "pr_comment",
    "pr_review_digest",
    "safe_merge",
    "ship",
    "link_pr",
    "pr_status",
    "pr_handoff",
    "release_note",
    "close_loop",
];

/// `tachi_memory` facade actions.
pub const TACHI_MEMORY_ACTIONS: &[&str] = &[
    "search",
    "get",
    "save",
    "briefing",
    "checkpoint",
    "alerts",
    "ask",
    "extract_facts",
    "consolidate",
];

/// Memory actions retired from the model-facing facade by #1689.
pub const TACHI_MEMORY_RETIRED_C2B_ACTIONS: &[&str] = &[
    "progress",
    "readiness",
    "delete",
    "gc",
    "doctor_scan",
    "ingest",
    "ingest_source",
    "pattern_feedback",
];

/// `tachi_tune` admin/operator actions. Introduced by #1426 to move route
/// and recall tuning out of daily `tachi_task` / `tachi_memory` schemas.
pub const TACHI_TUNE_ACTIONS: &[&str] = &[
    "route_simulate",
    "route_proposals",
    "route_review",
    "route_apply",
    "recall_simulate",
    "recall_proposals",
    "recall_review",
    "recall_apply",
];

/// `tachi_event` facade actions. Single source for `facade::tachi_event_action_schema`
/// and #1098's `action_effect` completeness test — previously each kept its own
/// inline copy of this list with no shared source to catch drift.
pub const TACHI_EVENT_ACTIONS: &[&str] = &[
    "emit",
    "query",
    "metrics",
    "project",
    "promote",
    "context",
    "a2a",
    "label_eval",
];

/// Closed local advisory-mailbox actions (#1751).
pub const TACHI_A2A_ACTIONS: &[&str] = &["respond", "status"];

/// `tachi_wiki` facade actions. Single source for `facade::tachi_wiki_action_schema`
/// and #1098's `action_effect` completeness test.
pub const TACHI_WIKI_ACTIONS: &[&str] = &["search", "browse", "read", "write"];

/// `tachi_component` facade actions. The schema inventory is independent from
/// the server's replay-effect map so the live-router ratchet detects drift.
pub(super) const TACHI_COMPONENT_ACTIONS: &[&str] = &["list", "show", "check", "plan"];

/// `tachi_skill` facade actions. Single source for `facade::tachi_skill_action_schema`
/// and #1098's `action_effect` completeness test.
pub const TACHI_SKILL_ACTIONS: &[&str] = &["discover", "run"];

/// `tachi_staff` facade actions. Single source for
/// `orchestration::tachi_staff_action_schema` and #1098's `action_effect`
/// completeness test.
pub const TACHI_STAFF_ACTIONS: &[&str] = &["start", "status", "cancel", "preflight", "result"];

/// Internal orchestrator actions (retired from MCP router; retained for
/// internal hard_state TODOs and handoffs accounting).
pub const TACHI_ORCHESTRATOR_ACTIONS: &[&str] = &[
    "todo_list",
    "todo_update",
    "handoff_write",
    "handoff_read",
    "recovery_briefing",
];

// ─── P3 (agent-facing action input cost): compact action-parameter guide ─────
//
// The flat model-facing facades advertise the UNION of every action's fields
// (`tachi_task` 65, `tachi_gh` 57, `tachi_memory` 43 top-level properties at the
// time of writing), so a caller cannot tell from the schema which fields its
// chosen action actually reads. This guide is the compact, action-scoped answer
// rendered into each facade's `action` property description at the MCP boundary.
//
// It is deliberately NOT a second field authority:
// - every note names an action that already exists in the canonical inventory
//   above, and fields that already exist on the owning params struct;
// - `action_parameter_notes_reference_real_actions_and_fields` cross-checks both
//   against the live inventory and the generated params schema, so the guide
//   cannot drift into teaching a non-existent action or field;
// - it is filtered by the visible/allow-listed actions at render time, so it can
//   never advertise an operator-forbidden action to a narrower profile;
// - it is intentionally CURATED, not exhaustive: it covers the most common
//   read/status shapes (owner-chosen examples) rather than multiplying the
//   schema with a full per-action matrix.
#[derive(Debug, Clone, Copy)]
pub struct ActionParamNote {
    pub action: &'static str,
    /// Fields the handler reads and that the action refuses without.
    pub required: &'static [&'static str],
    /// Fields the action reads when present; safely omitted otherwise.
    pub optional: &'static [&'static str],
    /// `(field, default)` pairs where the handler applies a default.
    pub defaults: &'static [(&'static str, &'static str)],
    /// A minimal, self-contained request body for this action.
    pub example: &'static str,
    /// Optional result-semantics precision clause, rendered verbatim. Used to
    /// keep the discovery text honest about what a state label does and does
    /// not prove.
    pub note: Option<&'static str>,
}

/// Shared result-semantics clause for the Task read actions. `COMPLETED` is an
/// execution/legacy inference, never independent acceptance; `state_basis` +
/// managed-run evidence and canonical adjudication are separate facts.
const TASK_COMPLETION_SEMANTICS_NOTE: &str = "COMPLETED is execution/legacy inference, not independent acceptance; read state_basis + managed-run evidence, and treat canonical adjudication as a separate fact";

/// Curated `tachi_task` notes.
///
/// KNOWN FROZEN-CONTRACT CONFLICT (reported, not worked around): `status` reads
/// an existing dispatch by `dispatch_id` as well as the flow-scoped lifecycle
/// model, so `dispatch_id` is part of the correct minimal guidance. But the
/// standard `tachi_task` action description is guarded by a blanket
/// `!description.contains("dispatch")` assertion (#1319-C2), which also matches
/// the legitimate read field `dispatch_id`. Rather than omit correct product
/// guidance to satisfy an over-broad substring guard, the field is kept; this is
/// reported for adjudication (narrow the guard to the authority phrases it
/// targets: `dispatch_reason` / `action=dispatch` / `explicit_user_request`).
pub const TACHI_TASK_ACTION_NOTES: &[ActionParamNote] = &[
    ActionParamNote {
        action: "status",
        required: &[],
        // Curated; the full optional set and defaults stay in each property's
        // own schema description.
        optional: &["flow_id", "issue_ref", "pr_ref", "dispatch_id"],
        defaults: &[],
        example: r#"{"action":"status","flow_id":"flow-1"}"#,
        note: Some(TASK_COMPLETION_SEMANTICS_NOTE),
    },
    ActionParamNote {
        action: "board",
        required: &[],
        optional: &["state_filter", "limit"],
        defaults: &[("limit", "20"), ("state_filter", "all")],
        example: r#"{"action":"board","state_filter":"active","limit":20}"#,
        note: Some(TASK_COMPLETION_SEMANTICS_NOTE),
    },
];

/// Curated `tachi_memory` notes.
pub const TACHI_MEMORY_ACTION_NOTES: &[ActionParamNote] = &[
    ActionParamNote {
        action: "search",
        required: &["query"],
        optional: &["scope", "top_k"],
        defaults: &[("top_k", "6"), ("scope", "all")],
        example: r#"{"action":"search","query":"briefing cap"}"#,
        note: None,
    },
    ActionParamNote {
        action: "get",
        required: &["id"],
        optional: &[],
        defaults: &[],
        example: r#"{"action":"get","id":"mem-abc123"}"#,
        note: None,
    },
    ActionParamNote {
        action: "save",
        required: &["text"],
        // Curated to the common shape; the full field set and each default
        // remain in the schema's own property descriptions.
        optional: &["title", "path"],
        defaults: &[("importance", "0.5"), ("scope", "memory")],
        example: r#"{"action":"save","text":"...","title":"..."}"#,
        note: None,
    },
    ActionParamNote {
        action: "briefing",
        required: &[],
        optional: &["project"],
        defaults: &[("compact", "true")],
        example: r#"{"action":"briefing","project":"tachi"}"#,
        note: None,
    },
];

/// Curated `tachi_gh` read notes.
pub const TACHI_GH_ACTION_NOTES: &[ActionParamNote] = &[
    ActionParamNote {
        action: "repo_view",
        required: &["repo"],
        optional: &[],
        defaults: &[],
        example: r#"{"action":"repo_view","repo":"owner/repo"}"#,
        note: None,
    },
    ActionParamNote {
        action: "issue_read",
        required: &["repo", "number"],
        optional: &[],
        defaults: &[],
        example: r#"{"action":"issue_read","repo":"owner/repo","number":123}"#,
        note: None,
    },
    ActionParamNote {
        action: "pr_read",
        required: &["repo", "number"],
        optional: &[],
        defaults: &[],
        example: r#"{"action":"pr_read","repo":"owner/repo","number":123}"#,
        note: None,
    },
];

/// Curated `tachi_staff` notes.
pub const TACHI_STAFF_ACTION_NOTES: &[ActionParamNote] = &[
    ActionParamNote {
        action: "status",
        required: &["dispatch_id"],
        optional: &[],
        defaults: &[],
        example: r#"{"action":"status","dispatch_id":"20260823T010101Z-custom-deadbeef"}"#,
        note: None,
    },
    ActionParamNote {
        action: "result",
        required: &["dispatch_id"],
        optional: &[],
        defaults: &[],
        example: r#"{"action":"result","dispatch_id":"20260823T010101Z-custom-deadbeef"}"#,
        note: Some(
            "Full UTF-8 report, at most 64 KiB; larger files refused. No staffing_reason required.",
        ),
    },
    ActionParamNote {
        action: "preflight",
        required: &["worker"],
        optional: &["profile"],
        defaults: &[],
        example: r#"{"action":"preflight","worker":"codex"}"#,
        note: None,
    },
];

/// Render the compact guide for the actions a caller can actually invoke.
///
/// `visible_actions` is the same allow-list the caller used to narrow the action
/// enum, so the guide can never mention an action the profile denies. Returns
/// `None` when no note applies (e.g. every noted action was denied), so the
/// caller can leave the description untouched.
pub fn render_action_param_guides(
    notes: &[ActionParamNote],
    visible_actions: &[&str],
) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut last_note: Option<&str> = None;
    for note in notes {
        if !visible_actions.contains(&note.action) {
            continue;
        }
        // Compact labels: the full per-field prose already lives in the schema
        // property descriptions, so the guide only adds required/optional shape,
        // keyed defaults, and a minimal example.
        // Compact: required set, a short curated optional set, a minimal example,
        // and (once) the result-semantics note. Defaults and the full optional
        // set are NOT restated — each field already has an action-tagged
        // property description in the schema.
        let mut line = format!("{} req=[{}]", note.action, note.required.join(","));
        if !note.optional.is_empty() {
            line.push_str(&format!(" opt=[{}]", note.optional.join(",")));
        }
        line.push_str(&format!(" ex={}", note.example));
        // Emit each distinct precision note once (status/board share one), so
        // the guide does not duplicate shared prose.
        if let Some(text) = note.note {
            if last_note != Some(text) {
                line.push_str(&format!(" note={text}"));
                last_note = Some(text);
            }
        }
        lines.push(line);
    }
    if lines.is_empty() {
        None
    } else {
        Some(lines.join(" | "))
    }
}

/// Soft ceilings for F0 monitoring (primary schema actions only).
pub const TACHI_TASK_PRIMARY_ACTION_SOFT_MAX: usize = 30;
pub const TACHI_MEMORY_ACTION_SOFT_MAX: usize = 25;
pub const TACHI_GH_ACTION_SOFT_MAX: usize = 20;

#[cfg(test)]
mod tests {
    use super::super::action_enums::{TachiTaskAction, TachiVerifyAction};
    use super::super::tune::TachiTuneAction;
    use super::*;

    #[test]
    fn f0_task_primary_does_not_advertise_gh_lifecycle() {
        let primary = TachiTaskAction::primary_wire_strings();
        for action in TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS {
            assert!(
                !primary.contains(action),
                "primary task schema must not advertise {action}; use tachi_gh"
            );
        }
        for action in TACHI_TASK_WORKCLAIM_ACTIONS {
            assert!(
                primary.contains(action),
                "#1253 WorkClaim action {action} must be advertised on tachi_task"
            );
        }
        for action in TACHI_TASK_RETIRED_C1A_ACTIONS {
            assert!(
                !primary.contains(action),
                "#1683 C1a / #1712 C1b retired task action {action} must not be advertised"
            );
        }
        for action in TACHI_TASK_RETIRED_C1C_ACTIONS {
            assert!(
                !primary.contains(action),
                "#1687 C1c retired task action {action} must not be advertised"
            );
        }
        assert_eq!(
            primary,
            [
                "intake",
                "claim",
                "heartbeat",
                "handoff",
                "release",
                "board",
                "status",
                "complete",
                "adjudicate",
                "brief",
            ]
        );
        // #1683 C1a plus #1712 C1b-1 plus #1713 contract the Task surface.
        // Keep this exact so the next inventory addition gets explicit review.
        assert_eq!(primary.len(), 10);
        assert!(primary.len() <= TACHI_TASK_PRIMARY_ACTION_SOFT_MAX);
    }

    #[test]
    fn f0_gh_owns_lifecycle_actions() {
        for action in TACHI_TASK_REMOVED_GH_LIFECYCLE_ACTIONS {
            assert!(
                TACHI_GH_ACTIONS.contains(action),
                "tachi_gh must own lifecycle action {action}"
            );
        }
        assert_eq!(TACHI_GH_ACTIONS.len(), 20);
        assert!(TACHI_GH_ACTIONS.len() <= TACHI_GH_ACTION_SOFT_MAX);
    }

    #[test]
    fn f0_memory_and_verify_counts() {
        assert_eq!(TACHI_MEMORY_ACTIONS.len(), 9);
        assert!(TACHI_MEMORY_ACTIONS.len() <= TACHI_MEMORY_ACTION_SOFT_MAX);
        assert_eq!(TachiVerifyAction::ALL.len(), 5);
    }

    #[test]
    fn f1689_memory_inventory_is_exactly_the_nine_survivors() {
        assert_eq!(
            TACHI_MEMORY_ACTIONS,
            &[
                "search",
                "get",
                "save",
                "briefing",
                "checkpoint",
                "alerts",
                "ask",
                "extract_facts",
                "consolidate",
            ],
            "#1689 must pin the literal final nine-action Memory inventory",
        );

        assert_eq!(
            crate::facade::memory::TachiMemoryAction::ALL
                .iter()
                .map(|action| action.as_str())
                .collect::<Vec<_>>(),
            TACHI_MEMORY_ACTIONS,
            "the typed Memory action enum and published inventory must not drift",
        );
    }

    #[test]
    fn f0_tune_inventory_count() {
        // #1426: route tuning leaves tachi_task (27 -> 23) and recall tuning
        // leaves tachi_memory (25 -> 21); #1688 then removes claim/release
        // (21 -> 19). The eight moved actions live only on
        // this admin/operator inventory; keep the count exact so the next
        // tuning action gets explicit review instead of quietly widening the
        // surface.
        assert_eq!(TACHI_TUNE_ACTIONS.len(), 8);
        assert_eq!(
            TACHI_TUNE_ACTIONS,
            TachiTuneAction::all_wire_strings().as_slice()
        );
    }

    /// P3 drift ratchet: the compact action-parameter guide may only name
    /// actions from the canonical inventory and fields that actually exist on
    /// the owning params schema, and every example must self-identify its
    /// action. This is what lets the guide be hand-curated without becoming a
    /// silent second authority.
    #[test]
    fn action_parameter_notes_reference_real_actions_and_fields() {
        use std::collections::BTreeSet;

        fn schema_properties_of(schema: rmcp::schemars::Schema) -> BTreeSet<String> {
            let value = serde_json::to_value(schema).expect("params schema serializes");
            value["properties"]
                .as_object()
                .expect("params properties")
                .keys()
                .cloned()
                .collect()
        }

        fn check(notes: &[ActionParamNote], actions: &[&str], props: &BTreeSet<String>) {
            for note in notes {
                assert!(
                    actions.contains(&note.action),
                    "guide note action '{}' is not in the canonical inventory",
                    note.action
                );
                for field in note
                    .required
                    .iter()
                    .chain(note.optional.iter())
                    .chain(note.defaults.iter().map(|(field, _)| field))
                {
                    assert!(
                        props.contains(*field),
                        "guide note '{}' references field '{field}' that is not on the params schema",
                        note.action
                    );
                }
                let example: serde_json::Value =
                    serde_json::from_str(note.example).expect("guide example is valid JSON");
                assert_eq!(
                    example["action"], note.action,
                    "guide example for '{}' must set action={}",
                    note.action, note.action
                );
            }
        }

        check(
            TACHI_TASK_ACTION_NOTES,
            &TachiTaskAction::primary_wire_strings(),
            &schema_properties_of(rmcp::schemars::schema_for!(super::super::TachiTaskParams)),
        );
        check(
            TACHI_MEMORY_ACTION_NOTES,
            TACHI_MEMORY_ACTIONS,
            &schema_properties_of(rmcp::schemars::schema_for!(super::super::TachiMemoryParams)),
        );
        check(
            TACHI_GH_ACTION_NOTES,
            TACHI_GH_ACTIONS,
            &schema_properties_of(rmcp::schemars::schema_for!(crate::gh::TachiGhParams)),
        );
        check(
            TACHI_STAFF_ACTION_NOTES,
            TACHI_STAFF_ACTIONS,
            &schema_properties_of(rmcp::schemars::schema_for!(super::super::TachiStaffParams)),
        );

        // Rendering is filtered by the visible allow-list.
        assert!(render_action_param_guides(TACHI_GH_ACTION_NOTES, &["repo_view"]).is_some());
        assert!(
            render_action_param_guides(TACHI_GH_ACTION_NOTES, &["safe_merge"]).is_none(),
            "a denied action must never be taught by the guide"
        );
        let rendered = render_action_param_guides(TACHI_STAFF_ACTION_NOTES, TACHI_STAFF_ACTIONS)
            .expect("staff notes render");
        assert!(rendered.contains("preflight") && rendered.contains("worker"));
    }

    #[test]
    fn f0_skill_and_staff_and_wiki_and_a2a_counts() {
        assert_eq!(TACHI_SKILL_ACTIONS, &["discover", "run"]);
        assert_eq!(
            TACHI_STAFF_ACTIONS,
            &["start", "status", "cancel", "preflight", "result"]
        );
        assert_eq!(TACHI_WIKI_ACTIONS, &["search", "browse", "read", "write"]);
        assert_eq!(TACHI_A2A_ACTIONS, &["respond", "status"]);
        assert_eq!(
            TACHI_EVENT_ACTIONS,
            &[
                "emit",
                "query",
                "metrics",
                "project",
                "promote",
                "context",
                "a2a",
                "label_eval",
            ]
        );
    }
}

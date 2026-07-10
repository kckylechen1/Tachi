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
pub(crate) const HEADER_PROJECT: &str = "x-tachi-project";

pub(crate) const META_PROFILE: &str = "tachiProfile";
pub(crate) const META_CLIENT: &str = "tachiClient";
pub(crate) const META_PROJECT: &str = "tachiProject";

pub(crate) fn enforce_session_project(
    tool_name: &str,
    arguments: &mut Option<JsonObject>,
    project: &str,
    transport_label: &str,
) -> Result<(), rmcp::ErrorData> {
    let args = arguments.get_or_insert_with(serde_json::Map::new);
    if let Some(explicit_project) = args.get("project") {
        if explicit_project.as_str() == Some(project) {
            return Ok(());
        }
        // Write isolation: reject routing into another project's DB. Cross-library
        // reads are safe because daemon-side project stores are opened read-only.
        if explicit_project.as_str().is_some()
            && explicit_project_can_cross_binding(tool_name, args)
        {
            return Ok(());
        }
        return Err(rmcp::ErrorData::invalid_params(
            format!(
                "{transport_label} project binding mismatch: session is bound to '{project}', but tool call requested project={explicit_project}"
            ),
            None,
        ));
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
    args.insert("project".to_string(), serde_json::json!(project));
    Ok(())
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
        | "memory_graph"
        | "get_edges"
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
            | "tachi_verify"
            | "tachi_gh"
            | "tachi_wiki"
            | "wiki_write"
            | "tachi_wiki_write"
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
            | "apply_recall_proposals"
            | "pattern_feedback"
            | "progress"
            | "readiness"
            | "recall_proposals"
            | "recall_simulate"
            | "review_recall_proposal"
            | "save"
            | "search"
    )
}

fn tachi_memory_action_allows_cross_project_read(action: &str) -> bool {
    matches!(
        action.to_ascii_lowercase().as_str(),
        "alerts"
            | "ask"
            | "briefing"
            | "consolidate"
            | "get"
            | "readiness"
            | "recall_simulate"
            | "search"
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(map: serde_json::Map<String, serde_json::Value>) -> Option<JsonObject> {
        Some(map)
    }

    fn map_from(pairs: &[(&str, serde_json::Value)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
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
        )
        .expect_err("cross-project write must fail");
        assert!(
            err.message.contains("project binding mismatch"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn bound_session_allows_cross_project_read_search() {
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
        )
        .expect("cross-project read must pass");
        assert_eq!(
            arguments
                .as_ref()
                .and_then(|a| a.get("project"))
                .and_then(|v| v.as_str()),
            Some("other")
        );
    }

    #[test]
    fn bound_session_injects_project_for_defaulting_write() {
        let mut arguments = args(map_from(&[
            ("action", json!("save")),
            ("text", json!("bound write")),
        ]));
        enforce_session_project("tachi_memory", &mut arguments, "sigil", "stdio proxy")
            .expect("inject project");
        assert_eq!(
            arguments
                .as_ref()
                .and_then(|a| a.get("project"))
                .and_then(|v| v.as_str()),
            Some("sigil")
        );
    }

    #[test]
    fn bound_session_does_not_force_project_on_global_scope() {
        let mut arguments = args(map_from(&[
            ("action", json!("search")),
            ("scope", json!("global")),
            ("query", json!("x")),
        ]));
        enforce_session_project("tachi_memory", &mut arguments, "sigil", "stdio proxy")
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

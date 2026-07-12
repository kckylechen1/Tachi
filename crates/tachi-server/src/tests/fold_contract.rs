//! #975-proof fold-contract harness (#757 Cut3-S1; reused by S2–S7).
//!
//! When a set of standalone tools is folded into a single verb facade, the
//! authorization surface must be *invariant*: every `(profile, legacy_tool)`
//! that could (or could not) reach a handler before the fold must have the
//! identical reachability for the `(profile, verb, action)` it became. This
//! module computes a callable matrix over the seven tool profiles by driving
//! the **real MCP `call_tool` choke point** (`call_tool_via_server`, the same
//! entry point `profile_tests::tool_profile` uses), so the harness catches
//! anything a unit-level `facade_action_allowed` check would miss (tool-level
//! visibility, the action gate, and their interaction).
//!
//! ## Usage (S2–S7)
//!
//! Build a `&[FoldPair]` mapping each legacy tool name to its canonical
//! `(verb, action)` and call [`assert_fold_matrix_equivalence`]. It asserts, for
//! every one of [`CONTRACT_PROFILES`], that the legacy tool and the folded
//! `verb(action=…)` have identical [`Reachability`]. For admin-only folds also
//! call [`assert_admin_only`] to pin the absolute expectation (callable only
//! under `admin`) so a fold that silently widened *both* the alias and the verb
//! to a bundle can't pass equivalence alone.

use super::{call_tool_via_server, make_server};
use serde_json::json;

/// The seven tool profiles the authorization matrix is enumerated over.
pub(crate) const CONTRACT_PROFILES: &[&str] = &[
    "observe",
    "remember",
    "coordinate",
    "operate",
    "standard",
    "delegate",
    "admin",
];

/// Whether a `(profile, tool, action)` call reaches its handler through the MCP
/// choke point, and if not, why it was stopped. "Callable" means the auth gates
/// (tool visibility + action gate) let the call through to the handler — the
/// handler may still return a business-logic error (missing required field,
/// unknown action); that is not an authorization outcome and is deliberately
/// collapsed into `Callable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reachability {
    /// Passed both gates and reached the handler.
    Callable,
    /// Tool is not visible to the profile ("tool not found").
    ToolHidden,
    /// Tool is visible but the action-level gate denied it.
    ActionDenied,
}

/// A legacy tool name and the canonical folded verb+action it maps to.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FoldPair {
    pub legacy_name: &'static str,
    pub canonical_tool: &'static str,
    pub canonical_action: &'static str,
}

/// Drive one `(profile, tool, action)` call through the real MCP `call_tool`
/// path and classify the authorization outcome.
pub(crate) async fn reachability(
    profile: &str,
    tool: &str,
    action: Option<&str>,
) -> Reachability {
    let server = make_server();
    server.set_tool_profile(Some(
        tachi_hub::parse_tool_profile(profile)
            .unwrap_or_else(|| panic!("profile '{profile}' should parse")),
    ));

    let mut args = serde_json::Map::new();
    if let Some(action) = action {
        args.insert("action".to_string(), json!(action));
    }

    // A JSON-RPC-level Err only occurs *after* the visibility + action gates
    // (they return Ok(...) results): a parameter-binding error means the call
    // was already authorized, so it counts as Callable.
    let result = match call_tool_via_server(server, tool, Some(args)).await {
        Ok(result) => result,
        Err(_) => return Reachability::Callable,
    };

    if result.is_error != Some(true) {
        return Reachability::Callable;
    }
    let message = result
        .content
        .first()
        .and_then(|content| content.as_text())
        .map(|text| text.text.as_str())
        .unwrap_or("");
    if message.contains("tool not found") {
        Reachability::ToolHidden
    } else if message.contains("not allowed") || message.contains("not available") {
        Reachability::ActionDenied
    } else {
        // Reached the handler (business-logic error such as a missing required
        // field or invalid action) — authorization succeeded.
        Reachability::Callable
    }
}

/// Assert that every legacy tool in `pairs` has, across all seven profiles, the
/// identical reachability as the folded `verb(action=…)` it maps to. This is
/// the per-cell fold invariant: the fold changed the surface shape, not who can
/// reach it.
pub(crate) async fn assert_fold_matrix_equivalence(pairs: &[FoldPair]) {
    for pair in pairs {
        for &profile in CONTRACT_PROFILES {
            let legacy = reachability(profile, pair.legacy_name, None).await;
            let folded =
                reachability(profile, pair.canonical_tool, Some(pair.canonical_action)).await;
            assert_eq!(
                legacy, folded,
                "fold changed reachability for profile='{profile}': legacy '{}' = {legacy:?} but \
                 {}(action='{}') = {folded:?}",
                pair.legacy_name, pair.canonical_tool, pair.canonical_action
            );
        }
    }
}

/// Assert that every pair in `pairs` — both the legacy alias and the folded
/// `verb(action=…)` — is callable ONLY under the `admin` profile and hidden
/// under every other profile. Complements [`assert_fold_matrix_equivalence`]:
/// equivalence alone would still pass if a fold widened *both* sides
/// identically, so admin-only folds pin the absolute expectation here.
pub(crate) async fn assert_admin_only(pairs: &[FoldPair]) {
    for pair in pairs {
        for &profile in CONTRACT_PROFILES {
            let expected = if profile == "admin" {
                Reachability::Callable
            } else {
                Reachability::ToolHidden
            };
            let legacy = reachability(profile, pair.legacy_name, None).await;
            assert_eq!(
                legacy, expected,
                "admin-only legacy alias '{}' must be {expected:?} for profile='{profile}'",
                pair.legacy_name
            );
            let folded =
                reachability(profile, pair.canonical_tool, Some(pair.canonical_action)).await;
            assert_eq!(
                folded, expected,
                "admin-only {}(action='{}') must be {expected:?} for profile='{profile}'",
                pair.canonical_tool, pair.canonical_action
            );
        }
    }
}

//! #975-proof fold-contract harness (#757 Cut3-S1; evolved for the v2 alias
//! retirement).
//!
//! When a set of standalone tools is folded into a single verb facade, the
//! authorization surface must be *invariant*: every `(profile, legacy_tool)`
//! that could (or could not) reach a handler before the fold must have the
//! identical reachability for the `(profile, verb, action)` it became. This
//! module computes a callable matrix over the seven tool profiles by driving
//! the **real MCP `call_tool` choke point** (`call_tool_on_server`, the same
//! entry point `profile_tests::tool_profile` uses), so the harness catches
//! anything a unit-level `facade_action_allowed` check would miss (tool-level
//! visibility, the action gate, and their interaction).
//!
//! ## v2 retirement evolution
//!
//! The S1 sandbox aliases expired (`remove_in_release` 1.10.0 ≤ 2.0.0) and
//! were deleted from the router, so "legacy alias vs verb" cell-for-cell
//! equivalence is no longer observable on the wire — the legacy side has no
//! route at all. The pre-fold equivalence guarantee did not disappear; it
//! moved to `sandbox_fold.rs`, which drives the *unchanged* pre-fold
//! `sandbox_ops` handlers directly as an oracle and demands byte-identical
//! output from the canonical facade. This harness now pins the retirement
//! shape of the authorization matrix itself:
//!
//! - [`assert_verb_actions_admin_only`]: every canonical `verb(action=…)` is
//!   `Callable` under `admin` and `ToolHidden` under the other six profiles.
//! - [`assert_legacy_names_unrouted_everywhere`]: every retired legacy name is
//!   `ToolHidden` under **all seven profiles, admin included** — a retired
//!   name must be rejected even for the admin profile that used to own it.
//!
//! ## Usage (S2–S7)
//!
//! Build a `&[FoldPair]` mapping each legacy tool name to its canonical
//! `(verb, action)` and call both asserts above after the aliases are retired,
//! together with the oracle-equality goldens in the fold's own test module.

use super::{call_tool_on_server, make_server};
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

/// A legacy tool name and the canonical folded verb+action it maps to. After a
/// retirement the `legacy_name` side is a tombstone: it must be unrouted, and
/// the pair records which canonical action replaced it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FoldPair {
    pub legacy_name: &'static str,
    pub canonical_tool: &'static str,
    pub canonical_action: &'static str,
}

/// Drive one `(tool, action)` call through the real MCP `call_tool` path on a
/// server whose profile was fixed by the caller, and classify the outcome.
pub(crate) async fn reachability(
    server: crate::MemoryServer,
    tool: &str,
    action: Option<&str>,
) -> Reachability {
    let mut args = serde_json::Map::new();
    if let Some(action) = action {
        args.insert("action".to_string(), json!(action));
    }

    // A JSON-RPC-level Err only occurs *after* the visibility + action gates
    // (they return Ok(...) results): a parameter-binding error means the call
    // was already authorized, so it counts as Callable.
    let result = match call_tool_on_server(server, tool, Some(args)).await {
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

/// Assert that every canonical `verb(action=…)` in `pairs` is callable ONLY
/// under the `admin` profile and hidden under every other profile. Absolute
/// pin (not mere equivalence): a fold that silently widened the verb to a
/// bundle fails here even if both sides widened identically.
pub(crate) async fn assert_verb_actions_admin_only(pairs: &[FoldPair]) {
    for &profile in CONTRACT_PROFILES {
        let server = make_server();
        server.set_tool_profile(Some(
            tachi_hub::parse_tool_profile(profile)
                .unwrap_or_else(|| panic!("profile '{profile}' should parse")),
        ));
        let expected = if profile == "admin" {
            Reachability::Callable
        } else {
            Reachability::ToolHidden
        };
        for pair in pairs {
            let folded = reachability(
                server.clone(),
                pair.canonical_tool,
                Some(pair.canonical_action),
            )
            .await;
            assert_eq!(
                folded, expected,
                "admin-only {}(action='{}') must be {expected:?} for profile='{profile}'",
                pair.canonical_tool, pair.canonical_action
            );
        }
    }
}

/// Assert that every legacy name in `pairs` is absent from the router
/// entirely: `ToolHidden` under ALL seven profiles, admin included. This is
/// the defining property of a completed retirement — the retired name must be
/// rejected even for the admin profile that used to own it, so a resurrected
/// zombie alias (or a retirement that only hid the name from non-admin
/// bundles) fails here.
pub(crate) async fn assert_legacy_names_unrouted_everywhere(pairs: &[FoldPair]) {
    for &profile in CONTRACT_PROFILES {
        let server = make_server();
        server.set_tool_profile(Some(
            tachi_hub::parse_tool_profile(profile)
                .unwrap_or_else(|| panic!("profile '{profile}' should parse")),
        ));
        for pair in pairs {
            let legacy = reachability(server.clone(), pair.legacy_name, None).await;
            assert_eq!(
                legacy,
                Reachability::ToolHidden,
                "retired legacy name '{}' must be unrouted for profile='{profile}' — a \
                 retired alias is rejected even for admin",
                pair.legacy_name
            );
        }
    }
}

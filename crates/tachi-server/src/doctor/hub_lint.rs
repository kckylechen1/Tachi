//! Hub capability lint: flags approved+enabled+non-open-health MCP
//! capabilities that are missing a `discovery_status` stamp in their
//! `definition` JSON.
//!
//! Context (#968 option B / #995): `capability_callable` treats a MISSING
//! `discovery_status` as callable for enabled+approved+(health != open) MCP
//! caps — a deliberate backward-compat carve-out for legacy/grandfathered
//! rows registered before the discovery-status contract existed (or via a
//! write path outside the register→review flow). That carve-out is safe
//! today because every in-band register+approve path stamps
//! `discovery_status` at review time, but it is safe **by convention, not
//! enforcement** — a future import/sync path that writes approved rows
//! verbatim could silently grandfather MCP caps into callable without ever
//! running discovery. This lint gives that silent case visibility (a
//! warning-tier finding), without changing `capability_callable` semantics
//! at all.

use memcore::HubCapability;

use super::DoctorWarning;
use tachi_hub::{health_status_allows_call, review_status_allows_call};

/// Scan a set of hub capabilities and return one [`DoctorWarning`] per
/// approved+enabled MCP capability, with `health_status` not equal to
/// `"open"` (mirrors `capability_callable`'s exact gate: only an open
/// circuit fails this check — `unknown`/`healthy`/`degraded` all pass),
/// whose `definition` JSON is missing (or does not parse to an object
/// containing) a `discovery_status` field. Read-only / pure — callers own
/// where the `HubCapability` rows come from (global store, project store,
/// or both).
pub fn hub_capability_discovery_status_warnings(caps: &[HubCapability]) -> Vec<DoctorWarning> {
    caps.iter()
        .filter(|cap| is_approved_enabled_mcp_missing_discovery_status(cap))
        .map(|cap| DoctorWarning {
            code: "mcp_cap_missing_discovery_status".to_string(),
            path: cap.id.clone(),
            message: format!(
                "MCP capability '{}' is enabled+approved+healthy but has no discovery_status \
                 stamp in its definition; it is grandfathered into `capability_callable` under \
                 the #968 option-B backward-compat carve-out instead of having passed live \
                 discovery",
                cap.id
            ),
            remediation: format!(
                "re-run discovery/review for '{}' (e.g. `hub review`) to stamp discovery_status, \
                 or confirm this is an intentionally-grandfathered legacy row",
                cap.id
            ),
        })
        .collect()
}

fn is_approved_enabled_mcp_missing_discovery_status(cap: &HubCapability) -> bool {
    if !cap.enabled {
        return false;
    }
    if !cap.cap_type.eq_ignore_ascii_case("mcp") {
        return false;
    }
    if !review_status_allows_call(&cap.review_status) {
        return false;
    }
    if !health_status_allows_call(&cap.health_status) {
        return false;
    }
    match serde_json::from_str::<serde_json::Value>(&cap.definition) {
        Ok(def) => def.get("discovery_status").is_none(),
        // Malformed JSON is a different, already-fail-closed problem for
        // capability_callable; not this lint's concern.
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(id: &str, cap_type: &str, definition: &str) -> HubCapability {
        HubCapability {
            id: id.to_string(),
            name: id.to_string(),
            cap_type: cap_type.to_string(),
            version: 1,
            description: String::new(),
            definition: definition.to_string(),
            enabled: true,
            review_status: "approved".to_string(),
            health_status: "healthy".to_string(),
            last_error: None,
            last_success_at: None,
            last_failure_at: None,
            fail_streak: 0,
            active_version: None,
            exposure_mode: "direct".to_string(),
            uses: 0,
            successes: 0,
            failures: 0,
            avg_rating: 0.0,
            last_used: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    // ── discrimination pair: fires vs silent ──────────────────────────────

    #[test]
    fn fires_for_approved_enabled_mcp_missing_discovery_status() {
        let missing = cap(
            "mcp:legacy-grandfathered",
            "mcp",
            r#"{"other_field":"value"}"#,
        );
        let warnings = hub_capability_discovery_status_warnings(&[missing]);
        assert_eq!(
            warnings.len(),
            1,
            "expected exactly one warning: {warnings:?}"
        );
        assert_eq!(warnings[0].code, "mcp_cap_missing_discovery_status");
        assert_eq!(warnings[0].path, "mcp:legacy-grandfathered");
        assert!(warnings[0].message.contains("mcp:legacy-grandfathered"));
    }

    #[test]
    fn silent_for_approved_enabled_mcp_with_stamped_discovery_status() {
        let stamped = cap("mcp:stamped", "mcp", r#"{"discovery_status":"ready"}"#);
        let warnings = hub_capability_discovery_status_warnings(&[stamped]);
        assert!(
            warnings.is_empty(),
            "expected no warnings for a stamped cap: {warnings:?}"
        );
    }

    // ── boundary coverage: lint must not fire outside its exact target shape ──

    #[test]
    fn silent_for_disabled_cap_missing_discovery_status() {
        let mut disabled = cap("mcp:disabled", "mcp", r#"{}"#);
        disabled.enabled = false;
        let warnings = hub_capability_discovery_status_warnings(&[disabled]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn silent_for_non_mcp_cap_missing_discovery_status() {
        let skill = cap("skill:code-review", "skill", r#"{}"#);
        let warnings = hub_capability_discovery_status_warnings(&[skill]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn fires_for_uppercase_mcp_cap_type_missing_discovery_status() {
        // #995 finding 2: cap_type case must not matter to the lint itself
        // (the pure function already uses eq_ignore_ascii_case; the bug was
        // in the SQL-backed collector upstream, covered separately in
        // tachi-server/src/bootstrap/manifest_cli.rs tests).
        let upper = cap("mcp:UPPER", "MCP", r#"{"other_field":"value"}"#);
        let warnings = hub_capability_discovery_status_warnings(&[upper]);
        assert_eq!(
            warnings.len(),
            1,
            "uppercase cap_type=MCP must still fire: {warnings:?}"
        );
        assert_eq!(warnings[0].path, "mcp:UPPER");
    }

    #[test]
    fn silent_for_pending_review_status() {
        let mut pending = cap("mcp:pending-review", "mcp", r#"{}"#);
        pending.review_status = "pending".to_string();
        let warnings = hub_capability_discovery_status_warnings(&[pending]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn silent_for_unhealthy_status() {
        let mut open = cap("mcp:open-circuit", "mcp", r#"{}"#);
        open.health_status = "open".to_string();
        let warnings = hub_capability_discovery_status_warnings(&[open]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn silent_for_malformed_definition_json() {
        // Malformed JSON already fails closed in capability_callable; this
        // lint only targets the specific "missing field" grandfather shape.
        let malformed = cap("mcp:malformed", "mcp", "not valid json{{{");
        let warnings = hub_capability_discovery_status_warnings(&[malformed]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn silent_for_non_string_discovery_status() {
        // Present-but-non-string already fails closed in capability_callable
        // (different failure mode from "missing"); out of scope for this lint.
        let non_string = cap("mcp:non-string", "mcp", r#"{"discovery_status":42}"#);
        let warnings = hub_capability_discovery_status_warnings(&[non_string]);
        assert!(warnings.is_empty());
    }

    #[test]
    fn multiple_caps_only_flag_the_matching_ones() {
        let missing = cap("mcp:missing", "mcp", r#"{}"#);
        let stamped = cap("mcp:stamped", "mcp", r#"{"discovery_status":"ready"}"#);
        let skill = cap("skill:noop", "skill", r#"{}"#);
        let warnings = hub_capability_discovery_status_warnings(&[missing, stamped, skill]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path, "mcp:missing");
    }
}

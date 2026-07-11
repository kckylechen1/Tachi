use memcore::HubCapability;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityVisibility {
    Listed,
    Discoverable,
    Hidden,
}

impl CapabilityVisibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Listed => "listed",
            Self::Discoverable => "discoverable",
            Self::Hidden => "hidden",
        }
    }
}

impl std::str::FromStr for CapabilityVisibility {
    type Err = ();

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "listed" | "public" | "show" => Ok(Self::Listed),
            "discoverable" | "on_demand" | "on-demand" | "call_only" | "call-only" => {
                Ok(Self::Discoverable)
            }
            "hidden" | "private" | "off" => Ok(Self::Hidden),
            _ => Err(()),
        }
    }
}

pub fn capability_visibility_from_definition(def: &Value) -> CapabilityVisibility {
    let raw = def
        .get("policy")
        .and_then(|p| p.get("visibility"))
        .and_then(|v| v.as_str())
        .or_else(|| def.get("visibility").and_then(|v| v.as_str()));

    raw.and_then(|value| value.parse::<CapabilityVisibility>().ok())
        .unwrap_or(CapabilityVisibility::Listed)
}

pub fn capability_visibility_for_cap(cap: &HubCapability) -> CapabilityVisibility {
    match serde_json::from_str::<Value>(&cap.definition) {
        Ok(def) => capability_visibility_from_definition(&def),
        Err(e) => {
            eprintln!(
                "[hub] invalid capability definition JSON for '{}': {e}; defaulting visibility=listed",
                cap.id
            );
            CapabilityVisibility::Listed
        }
    }
}

pub fn should_expose_skill_tool(cap: &HubCapability) -> bool {
    cap.enabled
        && cap.cap_type.eq_ignore_ascii_case("skill")
        && capability_visibility_for_cap(cap) == CapabilityVisibility::Listed
}

pub fn should_expose_mcp_tools(cap: &HubCapability) -> bool {
    cap.enabled
        && cap.cap_type.eq_ignore_ascii_case("mcp")
        && capability_visibility_for_cap(cap) == CapabilityVisibility::Listed
}

pub fn review_status_allows_call(review_status: &str) -> bool {
    review_status.eq_ignore_ascii_case("approved")
}

pub fn health_status_allows_call(health_status: &str) -> bool {
    !health_status.eq_ignore_ascii_case("open")
}

pub fn capability_callable(cap: &HubCapability) -> bool {
    if !cap.enabled {
        return false;
    }
    if !review_status_allows_call(&cap.review_status) {
        return false;
    }
    if !health_status_allows_call(&cap.health_status) {
        return false;
    }
    if !cap.cap_type.eq_ignore_ascii_case("mcp") {
        return true;
    }
    match serde_json::from_str::<serde_json::Value>(&cap.definition) {
        // Backward-compat (owner-ratified option B, #968 regression fix): a
        // capability that already passed enabled + approved + healthy is trusted
        // even without a discovery_status stamp (existing pre-#968 caps + caps
        // registered outside the review-discovery flow). Only an EXPLICIT
        // non-"ready" discovery_status, a non-string value, or malformed JSON
        // fails closed.
        Ok(def) => match def.get("discovery_status") {
            None => true,
            Some(v) => v.as_str() == Some("ready"),
        },
        Err(_) => false,
    }
}

/// JSON type name for a non-string `discovery_status` value, for diagnostic
/// messages only. Never echoes the value itself (#995 finding 3: an
/// attacker/misconfig-controlled `definition` blob could stuff a secret into
/// a non-string `discovery_status`, e.g. `{"discovery_status":{"token":"..."}}`
/// — the deny reason must say what shape it saw, not what it contained).
fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Sanitize a string before interpolating it into a deny-reason message:
/// strip control characters (including newlines, which could otherwise be
/// used to inject fake log lines / additional "reasons" into the message)
/// and cap length so a maliciously long `discovery_status` string can't
/// blow up the reason payload. Truncation is marked with a trailing `…`.
fn sanitize_reason_fragment(raw: &str) -> String {
    const MAX_LEN: usize = 64;
    let cleaned: String = raw.chars().filter(|c| !c.is_control()).collect();
    let mut truncated: String = cleaned.chars().take(MAX_LEN).collect();
    if cleaned.chars().count() > MAX_LEN {
        truncated.push('…');
    }
    truncated
}

/// Human-readable name of the FIRST gate that fails `capability_callable` for
/// `cap`, or `None` if the capability is callable. Mirrors the exact gate
/// order/logic in `capability_callable` (#995 residual 2) — this is a
/// diagnostic helper only; it does not change what is denied, only what the
/// resulting deny message can say about *why*.
pub fn capability_not_callable_reason(cap: &HubCapability) -> Option<String> {
    if !cap.enabled {
        return Some("enabled=false".to_string());
    }
    if !review_status_allows_call(&cap.review_status) {
        return Some(format!(
            "review_status='{}' (requires approved)",
            cap.review_status
        ));
    }
    if !health_status_allows_call(&cap.health_status) {
        return Some(format!(
            "health_status='{}' (must not be open)",
            cap.health_status
        ));
    }
    if !cap.cap_type.eq_ignore_ascii_case("mcp") {
        return None;
    }
    match serde_json::from_str::<serde_json::Value>(&cap.definition) {
        Ok(def) => match def.get("discovery_status") {
            None => None,
            Some(v) => match v.as_str() {
                Some("ready") => None,
                Some(other) => {
                    let sanitized = sanitize_reason_fragment(other);
                    Some(format!(
                        "discovery_status='{sanitized}' (requires 'ready' or absent)"
                    ))
                }
                // #995 finding 3: never echo the raw non-string value (it
                // may be an object/array carrying a secret) — only its JSON
                // type name.
                None => Some(format!(
                    "discovery_status has non-string type (got {}); must be a string",
                    json_type_name(v)
                )),
            },
        },
        Err(e) => Some(format!("definition is not valid JSON ({e})")),
    }
}

pub fn sanitize_skill_tool_name(skill_id: &str) -> Option<String> {
    let raw = skill_id.strip_prefix("skill:")?;
    let mut output = String::from("tachi_skill_");
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            output.push(c.to_ascii_lowercase());
        } else {
            output.push('_');
        }
    }
    while output.contains("__") {
        output = output.replace("__", "_");
    }
    Some(output.trim_end_matches('_').to_string())
}

pub fn make_text_tool_result(
    payload: &Value,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    let text = serde_json::to_string(payload)
        .map_err(|e| rmcp::ErrorData::internal_error(format!("serialize response: {e}"), None))?;
    serde_json::from_value(json!({
        "content": [{"type": "text", "text": text}],
        "isError": false
    }))
    .map_err(|e| rmcp::ErrorData::internal_error(format!("build MCP response: {e}"), None))
}

pub fn build_skill_tool_from_cap(
    cap: &HubCapability,
) -> Result<(String, rmcp::model::Tool), String> {
    let tool_name = sanitize_skill_tool_name(&cap.id)
        .ok_or_else(|| format!("Invalid skill id '{}'", cap.id))?;
    let def: Value = serde_json::from_str(&cap.definition)
        .map_err(|e| format!("Invalid skill definition JSON: {e}"))?;
    let input_schema = def
        .get("inputSchema")
        .cloned()
        .or_else(|| def.get("input_schema").cloned())
        .unwrap_or(json!({
            "type": "object",
            "properties": {
                "input": {"type": "string", "description": "Primary input for this skill"},
                "context": {"type": "string", "description": "Optional context"}
            },
            "additionalProperties": true
        }));
    let description = if cap.description.is_empty() {
        format!("Run skill {}", cap.name)
    } else {
        cap.description.clone()
    };
    let tool = serde_json::from_value::<rmcp::model::Tool>(json!({
        "name": tool_name,
        "description": description,
        "inputSchema": input_schema
    }))
    .map_err(|e| format!("Build skill tool failed: {e}"))?;
    Ok((tool_name, tool))
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

    #[test]
    fn visibility_accepts_policy_field_and_defaults_to_listed() {
        assert_eq!(
            capability_visibility_from_definition(&json!({"policy": {"visibility": "hidden"}})),
            CapabilityVisibility::Hidden
        );
        assert_eq!(
            capability_visibility_from_definition(&json!({"visibility": "on-demand"})),
            CapabilityVisibility::Discoverable
        );
        assert_eq!(
            capability_visibility_from_definition(&json!({})),
            CapabilityVisibility::Listed
        );
    }

    #[test]
    fn mcp_capability_requires_ready_discovery_to_be_callable() {
        let ready = cap("mcp:ready", "mcp", r#"{"discovery_status":"ready"}"#);
        let pending = cap("mcp:pending", "mcp", r#"{"discovery_status":"pending"}"#);

        assert!(capability_callable(&ready));
        assert!(!capability_callable(&pending));
    }

    #[test]
    fn mcp_capability_missing_discovery_status_is_callable_backward_compat() {
        // #968 option B (owner-ratified): a cap that already passed
        // enabled + approved + healthy but predates the discovery_status
        // stamp (or was registered outside the review-discovery flow) must
        // stay callable, not brick on upgrade.
        let missing = cap("mcp:missing", "mcp", r#"{"other_field":"value"}"#);

        assert!(capability_callable(&missing));
    }

    #[test]
    fn mcp_capability_malformed_definition_fails_closed() {
        let malformed = cap("mcp:malformed", "mcp", "not valid json{{{");

        assert!(!capability_callable(&malformed));
    }

    #[test]
    fn mcp_capability_non_string_discovery_status_fails_closed() {
        let non_string = cap("mcp:non-string", "mcp", r#"{"discovery_status":42}"#);

        assert!(!capability_callable(&non_string));
    }

    // ── capability_not_callable_reason (#995 residual 2) ──────────────────

    #[test]
    fn not_callable_reason_is_none_for_callable_cap() {
        let ready = cap("mcp:ready", "mcp", r#"{"discovery_status":"ready"}"#);
        assert_eq!(capability_not_callable_reason(&ready), None);

        let missing = cap("mcp:missing-ok", "mcp", r#"{}"#);
        assert_eq!(capability_not_callable_reason(&missing), None);
    }

    #[test]
    fn not_callable_reason_names_disabled_gate() {
        let mut disabled = cap("mcp:disabled", "mcp", r#"{"discovery_status":"ready"}"#);
        disabled.enabled = false;
        let reason = capability_not_callable_reason(&disabled).expect("should be denied");
        assert!(
            reason.contains("enabled=false"),
            "reason should name the enabled gate: {reason}"
        );
    }

    #[test]
    fn not_callable_reason_names_review_status_gate() {
        let mut pending = cap(
            "mcp:pending-review",
            "mcp",
            r#"{"discovery_status":"ready"}"#,
        );
        pending.review_status = "pending".to_string();
        let reason = capability_not_callable_reason(&pending).expect("should be denied");
        assert!(
            reason.contains("review_status='pending'"),
            "reason should name the review_status gate: {reason}"
        );
    }

    #[test]
    fn not_callable_reason_names_health_status_gate() {
        let mut open = cap("mcp:open-circuit", "mcp", r#"{"discovery_status":"ready"}"#);
        open.health_status = "open".to_string();
        let reason = capability_not_callable_reason(&open).expect("should be denied");
        assert!(
            reason.contains("health_status='open'"),
            "reason should name the health_status gate: {reason}"
        );
    }

    #[test]
    fn not_callable_reason_names_discovery_status_gate_for_explicit_pending() {
        let pending = cap("mcp:pending", "mcp", r#"{"discovery_status":"pending"}"#);
        let reason = capability_not_callable_reason(&pending).expect("should be denied");
        assert!(
            reason.contains("discovery_status='pending'"),
            "reason should name the discovery_status gate: {reason}"
        );
    }

    #[test]
    fn not_callable_reason_names_discovery_status_gate_for_non_string() {
        let non_string = cap("mcp:non-string", "mcp", r#"{"discovery_status":42}"#);
        let reason = capability_not_callable_reason(&non_string).expect("should be denied");
        assert!(
            reason.contains("discovery_status"),
            "reason should name the discovery_status gate: {reason}"
        );
    }

    // ── #995 finding 3: definition-controlled error injection/leak ────────

    #[test]
    fn not_callable_reason_never_echoes_raw_non_string_discovery_status_value() {
        // A non-string discovery_status could be an object carrying a
        // secret (codex review example: {"discovery_status":{"token":"secret"}}).
        // The deny reason must report only the JSON type, never the value.
        let leaky = cap(
            "mcp:leaky",
            "mcp",
            r#"{"discovery_status":{"token":"super-secret-value"}}"#,
        );
        let reason = capability_not_callable_reason(&leaky).expect("should be denied");
        assert!(
            !reason.contains("super-secret-value"),
            "reason must not leak the raw discovery_status value: {reason}"
        );
        assert!(
            !reason.contains("token"),
            "reason must not leak the raw discovery_status object shape: {reason}"
        );
        assert!(
            reason.contains("object"),
            "reason should name the JSON type (object): {reason}"
        );
    }

    #[test]
    fn not_callable_reason_reports_json_type_for_other_non_string_shapes() {
        let array_cap = cap("mcp:arr", "mcp", r#"{"discovery_status":[1,2,3]}"#);
        let reason = capability_not_callable_reason(&array_cap).expect("should be denied");
        assert!(reason.contains("array"), "expected array type: {reason}");
        assert!(
            !reason.contains('['),
            "must not echo raw array contents: {reason}"
        );

        let number_cap = cap("mcp:num", "mcp", r#"{"discovery_status":42}"#);
        let reason = capability_not_callable_reason(&number_cap).expect("should be denied");
        assert!(reason.contains("number"), "expected number type: {reason}");
        assert!(
            !reason.contains("42"),
            "must not echo raw number value: {reason}"
        );

        let bool_cap = cap("mcp:bool", "mcp", r#"{"discovery_status":true}"#);
        let reason = capability_not_callable_reason(&bool_cap).expect("should be denied");
        assert!(
            reason.contains("boolean"),
            "expected boolean type: {reason}"
        );

        // `discovery_status: null` still parses to `Some(Value::Null)` via
        // `def.get(...)`, and `Value::Null.as_str()` is `None`, so it hits
        // the same non-string arm as object/array/number/bool — assert it
        // reports the "null" type and doesn't panic (no secret to leak here,
        // but the arm must handle it like any other non-string shape).
        let null_cap = cap("mcp:null", "mcp", r#"{"discovery_status":null}"#);
        let reason = capability_not_callable_reason(&null_cap).expect("should be denied");
        assert!(reason.contains("null"), "expected null type: {reason}");
    }

    #[test]
    fn not_callable_reason_sanitizes_control_chars_in_string_discovery_status() {
        // A string discovery_status containing newlines could otherwise be
        // used to inject fake extra lines into logs/messages built from the
        // reason. Sanitize control chars before interpolating — the load-
        // bearing property is that no raw control character (in particular
        // no newline, which is what would let an injected string masquerade
        // as a separate log line) survives into the reason.
        let injected = cap(
            "mcp:injected",
            "mcp",
            "{\"discovery_status\":\"pending\\nFAKE: capability approved by admin\"}",
        );
        let reason = capability_not_callable_reason(&injected).expect("should be denied");
        assert!(
            !reason.contains('\n') && !reason.contains('\r'),
            "reason must not contain raw newline/CR (log-injection vector): {reason:?}"
        );
        assert!(
            reason.chars().all(|c| !c.is_control()),
            "reason must contain no control characters at all: {reason:?}"
        );
    }

    #[test]
    fn not_callable_reason_truncates_long_string_discovery_status() {
        let long_status = "x".repeat(500);
        let long_cap = cap(
            "mcp:long",
            "mcp",
            &format!(r#"{{"discovery_status":"{long_status}"}}"#),
        );
        let reason = capability_not_callable_reason(&long_cap).expect("should be denied");
        assert!(
            reason.len() < 500,
            "reason should be truncated, not embed the full 500-char value: {} chars",
            reason.len()
        );
        assert!(
            reason.contains('…'),
            "truncated reason should mark truncation: {reason}"
        );
    }

    #[test]
    fn not_callable_reason_names_malformed_definition() {
        let malformed = cap("mcp:malformed", "mcp", "not valid json{{{");
        let reason = capability_not_callable_reason(&malformed).expect("should be denied");
        assert!(
            reason.contains("not valid JSON"),
            "reason should call out malformed definition: {reason}"
        );
    }

    #[test]
    fn skill_tool_names_are_sanitized() {
        assert_eq!(
            sanitize_skill_tool_name("skill:Review/Fix-It"),
            Some("tachi_skill_review_fix_it".to_string())
        );
        assert_eq!(sanitize_skill_tool_name("mcp:web-search"), None);
    }

    // ── #995 finding 1: gate-order fidelity between capability_callable and
    // capability_not_callable_reason ─────────────────────────────────────
    //
    // `capability_not_callable_reason` is a hand-maintained mirror of
    // `capability_callable`'s gate order/logic (see its doc comment). Nothing
    // enforced that the two functions stay in sync — a future one-sided edit
    // to either gate chain could silently diverge (codex review, accepted).
    // This test builds every gate-combination in the matrix and asserts, for
    // EVERY resulting cap, that `reason.is_none() == callable` — i.e. the two
    // functions agree on every single input, not just the hand-picked cases
    // covered by the unit tests above.

    #[derive(Clone, Copy)]
    enum DiscoveryStatusCase {
        Absent,
        Ready,
        Pending,
        NonString,
        MalformedJson,
    }

    fn build_definition(discovery: DiscoveryStatusCase) -> String {
        match discovery {
            DiscoveryStatusCase::Absent => r#"{"other_field":"value"}"#.to_string(),
            DiscoveryStatusCase::Ready => r#"{"discovery_status":"ready"}"#.to_string(),
            DiscoveryStatusCase::Pending => r#"{"discovery_status":"pending"}"#.to_string(),
            DiscoveryStatusCase::NonString => r#"{"discovery_status":42}"#.to_string(),
            DiscoveryStatusCase::MalformedJson => "not valid json{{{".to_string(),
        }
    }

    #[test]
    fn callable_and_not_callable_reason_agree_on_every_gate_combination() {
        let enabled_values = [true, false];
        let review_statuses = ["approved", "pending", "rejected"];
        let health_statuses = ["healthy", "open", "unknown", "degraded"];
        // Includes case variants ("mcp"/"MCP") and non-MCP types (the
        // cap_type gate is a short-circuit for non-MCP caps in both fns).
        let cap_types = ["mcp", "MCP", "skill", "plugin"];
        let discovery_cases = [
            DiscoveryStatusCase::Absent,
            DiscoveryStatusCase::Ready,
            DiscoveryStatusCase::Pending,
            DiscoveryStatusCase::NonString,
            DiscoveryStatusCase::MalformedJson,
        ];

        let mut checked = 0usize;
        for &enabled in &enabled_values {
            for &review_status in &review_statuses {
                for &health_status in &health_statuses {
                    for &cap_type in &cap_types {
                        for &discovery in &discovery_cases {
                            let discovery_tag = discovery as u8;
                            let mut c = cap(
                                &format!("matrix:{cap_type}-{discovery_tag}"),
                                cap_type,
                                &build_definition(discovery),
                            );
                            c.enabled = enabled;
                            c.review_status = review_status.to_string();
                            c.health_status = health_status.to_string();

                            let callable = capability_callable(&c);
                            let reason = capability_not_callable_reason(&c);
                            assert_eq!(
                                reason.is_none(),
                                callable,
                                "gate-order divergence for enabled={enabled} \
                                 review_status={review_status} health_status={health_status} \
                                 cap_type={cap_type} discovery_case={}: callable={callable} \
                                 reason={reason:?}",
                                discovery as u8
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        // Sanity: make sure the matrix actually ran (guards against a
        // refactor accidentally emptying one of the arrays above).
        assert_eq!(checked, 2 * 3 * 4 * 4 * 5);
    }

    /// First-failing-gate precedence: a cap failing MULTIPLE gates at once
    /// (disabled AND pending review) must report the FIRST gate in order
    /// (`enabled`), not a later one — pinning the exact precedence both
    /// functions must keep in lockstep.
    #[test]
    fn not_callable_reason_reports_first_failing_gate_for_multi_failure_cap() {
        let mut multi_failure = cap(
            "mcp:multi-failure",
            "mcp",
            r#"{"discovery_status":"pending"}"#,
        );
        multi_failure.enabled = false;
        multi_failure.review_status = "pending".to_string();
        multi_failure.health_status = "open".to_string();

        assert!(!capability_callable(&multi_failure));
        let reason = capability_not_callable_reason(&multi_failure).expect("should be denied");
        assert!(
            reason.contains("enabled=false"),
            "reason should name the FIRST failing gate (enabled), not review_status/health/\
             discovery_status which also fail on this cap: {reason}"
        );
        assert!(
            !reason.contains("review_status") && !reason.contains("health_status"),
            "reason should only name the first gate, not stack multiple: {reason}"
        );
    }
}

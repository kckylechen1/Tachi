use serde_json::json;

use super::connection::sanitize_receipt_field;
use super::permission::{classify_acp_request_kind, native_permission_decision, AcpRequestKind};

/// Realistic ACP `PermissionOption` list: every option carries the typed
/// `kind` the real protocol sends (`allow_once` / `reject_once`), not just a
/// human-readable id/name. Used by every test so both ALLOW and DENY (which
/// read only `kind`, never `optionId`/`name`) have something legal to select.
fn read_write_options() -> serde_json::Value {
    json!([
        {"optionId": "deny", "name": "Deny", "kind": "reject_once"},
        {"optionId": "allow", "name": "Allow", "kind": "allow_once"}
    ])
}

#[test]
fn native_permission_approves_read_like_request() {
    let response = native_permission_decision(
        "approve-reads",
        &json!({
            "toolCall": {
                "kind": "read",
                "title": "Read src/lib.rs"
            },
            "options": read_write_options(),
        }),
    )
    .response;

    assert_eq!(response["outcome"]["outcome"], json!("selected"));
    assert_eq!(response["outcome"]["optionId"], json!("allow"));
}

#[test]
fn native_permission_denies_write_like_request() {
    let response = native_permission_decision(
        "approve-reads",
        &json!({
            "toolCall": {
                "kind": "edit",
                "title": "Write src/lib.rs"
            },
            "options": read_write_options(),
        }),
    )
    .response;

    assert_eq!(response["outcome"]["outcome"], json!("selected"));
    assert_eq!(response["outcome"]["optionId"], json!("deny"));
}

#[test]
fn unknown_request_kind_is_denied_with_receipt_naming_the_kind() {
    // An unrecognized ACP ToolKind (fail-closed): a substring authorizer would
    // read the "read" in the title and approve; the taxonomy denies.
    let params = json!({
        "toolCall": {
            "kind": "fabricate",
            "title": "read-only preview then fabricate output"
        },
        "options": read_write_options(),
    });

    let decision = native_permission_decision("approve-reads", &params);
    assert!(!decision.allowed, "unknown kind must be denied");
    assert_eq!(decision.kind, AcpRequestKind::Unknown);
    // The receipt names the request kind that was denied.
    assert_eq!(decision.kind.as_str(), "unknown");
    assert_eq!(decision.raw_kind, "fabricate");
    assert_eq!(decision.response["outcome"]["optionId"], json!("deny"));

    // A missing kind is also Unknown -> denied, receipt names it explicitly.
    let missing = json!({ "options": read_write_options() });
    let decision = native_permission_decision("approve-reads", &missing);
    assert!(!decision.allowed);
    assert_eq!(decision.kind, AcpRequestKind::Unknown);
    assert_eq!(decision.raw_kind, "<missing>");
}

/// #894 hardening (leader adjudication, cross-vendor review): only the
/// canonical `params.toolCall.kind` may ever classify as `ReadOp` (i.e. be
/// ALLOW-capable). A fallback kind-shaped field (`toolCall.tool_kind`, or a
/// stray top-level `kind`) can enrich a DENY receipt but must never promote a
/// request to allow, whether or not a canonical kind is present.
#[test]
fn only_canonical_tool_call_kind_can_produce_allow() {
    // Case 1: canonical toolCall.kind says "write" (edit); a top-level
    // `kind: "read"` field is also present, as if trying to smuggle a
    // read-like fallback past the classifier. The canonical field wins and
    // the request is denied.
    let write_with_read_fallback = json!({
        "toolCall": { "kind": "edit" },
        "kind": "read",
        "options": read_write_options(),
    });
    let decision = native_permission_decision("approve-reads", &write_with_read_fallback);
    assert!(
        !decision.allowed,
        "a write canonical kind must deny even with a read-shaped fallback field present"
    );

    // Case 2: no toolCall.kind at all (only a stray top-level `kind: "read"`).
    // With no canonical field, the classification must be Unknown -> DENY,
    // regardless of what any fallback field claims.
    let only_fallback_kind = json!({
        "kind": "read",
        "options": read_write_options(),
    });
    let decision = native_permission_decision("approve-reads", &only_fallback_kind);
    assert!(
        !decision.allowed,
        "a fallback-only 'read' kind with no canonical toolCall.kind must deny"
    );
    assert_eq!(decision.kind, AcpRequestKind::Unknown);
    assert_eq!(decision.raw_kind, "<missing>");

    let (variant, raw) = classify_acp_request_kind(&only_fallback_kind);
    assert_eq!(variant, AcpRequestKind::Unknown);
    assert_eq!(raw, "<missing>");
}

/// Crafted-conflict case: the canonical field itself says "read" (which would
/// normally classify ReadOp/allow-capable), but a fallback kind field
/// (`toolCall.tool_kind`) disagrees and claims "write". The classifier must
/// not sail this through as an allow just because the canonical field looks
/// benign — the conflict itself forces Unknown -> DENY.
#[test]
fn crafted_conflicting_kind_fields_are_denied() {
    let conflicting = json!({
        "toolCall": { "kind": "read", "tool_kind": "write" },
        "options": read_write_options(),
    });

    let (variant, raw) = classify_acp_request_kind(&conflicting);
    assert_eq!(
        variant,
        AcpRequestKind::Unknown,
        "disagreeing canonical vs fallback kind must force Unknown, not ReadOp"
    );
    assert_eq!(raw, "read", "raw_kind still reports the canonical field");

    let decision = native_permission_decision("approve-reads", &conflicting);
    assert!(!decision.allowed, "a conflicting request must be denied");
}

#[test]
fn classifier_buckets_kinds_into_taxonomy() {
    // ACP spec ToolKind read values only: read, search.
    for kind in ["read", "search"] {
        let (variant, _) = classify_acp_request_kind(&json!({"toolCall": {"kind": kind}}));
        assert_eq!(
            variant,
            AcpRequestKind::ReadOp,
            "expected read_op for {kind}"
        );
    }
    for kind in ["write", "edit", "delete", "remove", "create", "move"] {
        let (variant, _) = classify_acp_request_kind(&json!({"toolCall": {"kind": kind}}));
        assert_eq!(
            variant,
            AcpRequestKind::WriteOp,
            "expected write_op for {kind}"
        );
    }
    for kind in ["execute", "exec", "terminal", "shell", "run"] {
        let (variant, _) = classify_acp_request_kind(&json!({"toolCall": {"kind": kind}}));
        assert_eq!(
            variant,
            AcpRequestKind::ExecuteOp,
            "expected execute_op for {kind}"
        );
    }
    // grep/list/view/find were legacy-parity carryover, not ACP spec values;
    // they are now Unknown->DENY (#894 safety-over-parity), alongside the
    // genuinely-unrecognized ACP kinds and any bogus value.
    for kind in [
        "grep",
        "list",
        "view",
        "find",
        "fetch",
        "think",
        "switch_mode",
        "other",
        "bogus",
    ] {
        let (variant, _) = classify_acp_request_kind(&json!({"toolCall": {"kind": kind}}));
        assert_eq!(
            variant,
            AcpRequestKind::Unknown,
            "expected unknown for {kind}"
        );
    }
}

/// #894 hardening: DENY must select strictly by the ACP typed
/// `PermissionOption.kind` field, never by substring-matching the option's
/// `optionId`/`name`. A deceptively-named allow option (its `name` contains
/// the word "reject" even though its typed `kind` is `allow_once`) must never
/// be selected on the DENY path; with no legal reject-kind option present,
/// the only ACP-legal refusal is the `cancelled` outcome.
#[test]
fn deny_never_selects_by_deceptive_option_name() {
    let params = json!({
        "toolCall": { "kind": "edit" }, // write -> must deny
        "options": [
            {"optionId": "allow-once", "name": "Do not reject", "kind": "allow_once"}
        ],
    });

    let decision = native_permission_decision("approve-reads", &params);
    assert!(!decision.allowed, "write kind must be denied");
    assert_eq!(
        decision.response["outcome"]["outcome"],
        json!("cancelled"),
        "no reject-kind option exists, so the only legal refusal is 'cancelled', not selecting the deceptive allow option"
    );
    assert!(
        decision.response["outcome"].get("optionId").is_none(),
        "the deceptive 'allow-once' option must never be selected on the DENY path: {:?}",
        decision.response
    );
}

/// #894 S0 round 2: ALLOW must also select strictly by typed `kind`, never by
/// substring-matching `optionId`/`name`. Deceptive names on both options
/// (an "approve"-looking name on the reject-kind option, a "reject"-looking
/// name on the allow-kind option) must not confuse selection — the correct
/// allow_once-kind option is still chosen.
#[test]
fn allow_selects_by_kind_even_with_deceptive_option_names() {
    let params = json!({
        "toolCall": { "kind": "read" }, // ReadOp -> allow-capable
        "options": [
            {"optionId": "trap-approve", "name": "Approve this request", "kind": "reject_once"},
            {"optionId": "real-allow", "name": "Please reject me", "kind": "allow_once"}
        ],
    });

    let decision = native_permission_decision("approve-reads", &params);
    assert!(decision.allowed, "read kind must be allow-capable");
    assert_eq!(
        decision.response["outcome"]["optionId"],
        json!("real-allow"),
        "must select by kind (allow_once), not by the misleading name text: {:?}",
        decision.response
    );
}

/// #894 S0 round 2: an allow verdict with only reject-kind options present
/// must not fall back to selecting one of them (or anything else) — the only
/// legal outcome is `cancelled`.
#[test]
fn allow_verdict_with_only_reject_kind_options_is_cancelled() {
    let params = json!({
        "toolCall": { "kind": "read" }, // ReadOp -> allow-capable
        "options": [
            {"optionId": "deny-once", "name": "Deny", "kind": "reject_once"},
            {"optionId": "deny-always", "name": "Always Deny", "kind": "reject_always"}
        ],
    });

    let decision = native_permission_decision("approve-reads", &params);
    assert!(decision.allowed, "read kind must be allow-capable");
    assert_eq!(
        decision.response["outcome"]["outcome"],
        json!("cancelled"),
        "no allow-kind option exists, so the only legal outcome is 'cancelled': {:?}",
        decision.response
    );
    assert!(decision.response["outcome"].get("optionId").is_none());
}

/// #894 S0 round 2: `allow_once` is preferred over `allow_always` when both
/// are present (least-persistent grant).
#[test]
fn allow_prefers_allow_once_over_allow_always() {
    let params = json!({
        "toolCall": { "kind": "read" },
        "options": [
            {"optionId": "persistent", "name": "Always Allow", "kind": "allow_always"},
            {"optionId": "once", "name": "Allow Once", "kind": "allow_once"}
        ],
    });

    let decision = native_permission_decision("approve-reads", &params);
    assert!(decision.allowed);
    assert_eq!(decision.response["outcome"]["optionId"], json!("once"));
}

/// #894 hardening: `raw_tool_kind` is attacker-influenced (it comes off the
/// child ACP agent's own request), so it must be sanitized before it lands in
/// a trajectory event or a tracing log line — control characters stripped,
/// length capped at 64 chars.
#[test]
fn sanitize_receipt_field_strips_control_chars_and_truncates() {
    // Control characters (including an embedded newline that could otherwise
    // forge extra log lines) are stripped, not merely escaped.
    let injected = "read\n[FAKE] audit: request approved\t\u{0007}";
    let sanitized = sanitize_receipt_field(injected);
    assert!(!sanitized.contains('\n'));
    assert!(!sanitized.contains('\t'));
    assert!(!sanitized.chars().any(|ch| ch.is_control()));
    assert!(sanitized.contains("read"));
    assert!(sanitized.contains("FAKE"));

    // Oversized field is capped at 64 chars.
    let oversized = "x".repeat(500);
    let sanitized = sanitize_receipt_field(&oversized);
    assert_eq!(sanitized.chars().count(), 64);

    // Empty / all-control input degrades to a placeholder, not an empty string
    // that could be confused with a missing field.
    assert_eq!(sanitize_receipt_field(""), "<empty>");
    assert_eq!(sanitize_receipt_field("\n\t\u{0007}"), "<empty>");
}

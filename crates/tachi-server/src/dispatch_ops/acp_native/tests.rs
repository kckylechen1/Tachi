use serde_json::json;
use std::sync::{Mutex, MutexGuard, OnceLock};

use super::connection::sanitize_receipt_field;
use super::permission::{
    classify_acp_request_kind, native_permission_decision, permission_request_is_read_like,
    AcpRequestKind,
};

/// Serializes tests that read `TACHI_ACP_PERMISSION_HEURISTIC`, so the legacy
/// flag test cannot race a concurrent taxonomy test in the same binary.
fn heuristic_env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct HeuristicEnvGuard {
    original: Option<String>,
}

impl HeuristicEnvGuard {
    fn set(value: &str) -> Self {
        let original = std::env::var("TACHI_ACP_PERMISSION_HEURISTIC").ok();
        std::env::set_var("TACHI_ACP_PERMISSION_HEURISTIC", value);
        Self { original }
    }

    fn clear() -> Self {
        let original = std::env::var("TACHI_ACP_PERMISSION_HEURISTIC").ok();
        std::env::remove_var("TACHI_ACP_PERMISSION_HEURISTIC");
        Self { original }
    }
}

impl Drop for HeuristicEnvGuard {
    fn drop(&mut self) {
        match self.original.as_ref() {
            Some(value) => std::env::set_var("TACHI_ACP_PERMISSION_HEURISTIC", value),
            None => std::env::remove_var("TACHI_ACP_PERMISSION_HEURISTIC"),
        }
    }
}

/// Realistic ACP `PermissionOption` list: every option carries the typed
/// `kind` the real protocol sends (`allow_once` / `reject_once`), not just a
/// human-readable id/name. Used by every test so the DENY path (which reads
/// only `kind`, never `optionId`/`name`) has something legal to select.
fn read_write_options() -> serde_json::Value {
    json!([
        {"optionId": "deny", "name": "Deny", "kind": "reject_once"},
        {"optionId": "allow", "name": "Allow", "kind": "allow_once"}
    ])
}

#[test]
fn native_permission_approves_read_like_request() {
    let _lock = heuristic_env_lock();
    let _env = HeuristicEnvGuard::clear();
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
    let _lock = heuristic_env_lock();
    let _env = HeuristicEnvGuard::clear();
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
    let _lock = heuristic_env_lock();
    let _env = HeuristicEnvGuard::clear();
    // An unrecognized ACP ToolKind (fail-closed): the substring authorizer would
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
    assert!(!decision.heuristic);
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
    let _lock = heuristic_env_lock();
    let _env = HeuristicEnvGuard::clear();

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
/// benign — the conflict itself forces Unknown -> DENY. This also happens to
/// be a case where the deprecated legacy heuristic agrees to deny (the
/// serialized params contain both "read" and "write" substrings, so its
/// read-like-and-not-write-like rule fails too) — both authorizers converge
/// on DENY for a conflicting request.
#[test]
fn crafted_conflicting_kind_fields_are_denied_by_both_authorizers() {
    let _lock = heuristic_env_lock();
    let _env = HeuristicEnvGuard::clear();

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

    let taxonomy_decision = native_permission_decision("approve-reads", &conflicting);
    assert!(
        !taxonomy_decision.allowed,
        "typed taxonomy must deny a conflicting request"
    );

    assert!(
        !permission_request_is_read_like(&conflicting),
        "legacy heuristic also denies: both 'read' and 'write' substrings are present"
    );
}

#[test]
fn taxonomy_matches_legacy_verdict_for_every_keyword_case() {
    let _lock = heuristic_env_lock();
    let _env = HeuristicEnvGuard::clear();
    // Keywords where the typed taxonomy and the legacy substring heuristic
    // agree on the allow/deny verdict.
    let converged_keywords = [
        // ACP spec read kinds
        "read", "search",
        // legacy write/exec keywords
        "write", "edit", "delete", "remove", "terminal", "shell", "exec", "create",
        // additional real ACP ToolKind values
        "move", "execute", "fetch", "think", "switch_mode", "other",
    ];

    for kind in converged_keywords {
        let params = json!({
            "toolCall": { "kind": kind },
            "options": read_write_options(),
        });

        let legacy_read = permission_request_is_read_like(&params);
        let (taxonomy_kind, _) = classify_acp_request_kind(&params);
        let taxonomy_read = taxonomy_kind == AcpRequestKind::ReadOp;
        assert_eq!(
            legacy_read, taxonomy_read,
            "verdict parity broke for kind '{kind}': legacy read-like={legacy_read}, taxonomy={taxonomy_kind:?}"
        );

        let response = native_permission_decision("approve-reads", &params).response;
        let expected = if taxonomy_read { "allow" } else { "deny" };
        assert_eq!(
            response["outcome"]["optionId"],
            json!(expected),
            "decision outcome parity broke for kind '{kind}'"
        );
    }

    // #894 hardening: grep/list/view/find were legacy-substring-heuristic
    // carryover, not real ACP `ToolKind` values. Safety beats parity here —
    // this is an INTENTIONAL behavior change: the legacy heuristic still
    // treats them as read-like (it does substring matching over the whole
    // request, and these words look read-like), but the typed taxonomy now
    // denies them as Unknown. Document, don't hide, the divergence.
    let intentionally_diverged_keywords = ["grep", "list", "view", "find"];
    for kind in intentionally_diverged_keywords {
        let params = json!({
            "toolCall": { "kind": kind },
            "options": read_write_options(),
        });

        assert!(
            permission_request_is_read_like(&params),
            "legacy heuristic still treats '{kind}' as read-like (unchanged, deprecated behavior)"
        );
        let (taxonomy_kind, _) = classify_acp_request_kind(&params);
        assert_eq!(
            taxonomy_kind,
            AcpRequestKind::Unknown,
            "'{kind}' is not an ACP spec ToolKind; the taxonomy must now deny it (#894 safety-over-parity)"
        );
        let decision = native_permission_decision("approve-reads", &params);
        assert!(
            !decision.allowed,
            "'{kind}' must be denied under the typed taxonomy even though legacy approved it"
        );
    }
}

#[test]
fn classifier_buckets_kinds_into_taxonomy() {
    // ACP spec ToolKind read values only: read, search.
    for kind in ["read", "search"] {
        let (variant, _) = classify_acp_request_kind(&json!({"toolCall": {"kind": kind}}));
        assert_eq!(variant, AcpRequestKind::ReadOp, "expected read_op for {kind}");
    }
    for kind in ["write", "edit", "delete", "remove", "create", "move"] {
        let (variant, _) = classify_acp_request_kind(&json!({"toolCall": {"kind": kind}}));
        assert_eq!(variant, AcpRequestKind::WriteOp, "expected write_op for {kind}");
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
        assert_eq!(variant, AcpRequestKind::Unknown, "expected unknown for {kind}");
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
    let _lock = heuristic_env_lock();
    let _env = HeuristicEnvGuard::clear();

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

#[test]
fn legacy_flag_restores_old_substring_behavior() {
    let _lock = heuristic_env_lock();

    // A request the taxonomy denies (unknown kind) but the substring heuristic
    // approves (title contains "read", no write keyword). This is exactly the
    // fail-open case the taxonomy closes.
    let params = json!({
        "toolCall": {
            "kind": "other",
            "title": "read the file"
        },
        "options": read_write_options(),
    });

    // Default (taxonomy) path: denied.
    {
        let _env = HeuristicEnvGuard::clear();
        let decision = native_permission_decision("approve-reads", &params);
        assert!(!decision.allowed, "taxonomy must deny unknown kind");
        assert!(!decision.heuristic);
    }

    // Legacy flag: the old substring authorizer approves, and the decision is
    // stamped as heuristic (the loud warning fires from this path).
    {
        let _env = HeuristicEnvGuard::set("legacy");
        let decision = native_permission_decision("approve-reads", &params);
        assert!(
            decision.allowed,
            "legacy heuristic should restore the old approve-on-substring behavior"
        );
        assert!(decision.heuristic);
        assert_eq!(decision.response["outcome"]["optionId"], json!("allow"));
    }
}

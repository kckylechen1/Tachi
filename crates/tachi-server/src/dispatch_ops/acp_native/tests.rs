use serde_json::json;
use std::sync::{Mutex, MutexGuard, OnceLock};

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

fn read_write_options() -> serde_json::Value {
    json!([
        {"optionId": "deny", "name": "Deny"},
        {"optionId": "allow", "name": "Allow"}
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
            "options": [
                {"optionId": "deny", "name": "Deny"},
                {"optionId": "allow", "name": "Allow"}
            ]
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
            "options": [
                {"optionId": "allow", "name": "Allow"},
                {"optionId": "deny", "name": "Deny"}
            ]
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

#[test]
fn taxonomy_matches_legacy_verdict_for_every_keyword_case() {
    let _lock = heuristic_env_lock();
    let _env = HeuristicEnvGuard::clear();
    // Legacy read + write keyword vocabularies plus the real ACP ToolKind enum.
    // For each keyword-as-kind, the typed taxonomy must reach the same
    // allow/deny verdict the substring heuristic did (parity), while reading
    // only the structured field.
    let keywords = [
        // legacy read keywords
        "read", "search", "grep", "list", "view", "find",
        // legacy write/exec keywords
        "write", "edit", "delete", "remove", "terminal", "shell", "exec", "create",
        // additional real ACP ToolKind values
        "move", "execute", "fetch", "think", "switch_mode", "other",
    ];

    for kind in keywords {
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

        // And the full decision outcome agrees too.
        let response = native_permission_decision("approve-reads", &params).response;
        let expected = if taxonomy_read { "allow" } else { "deny" };
        assert_eq!(
            response["outcome"]["optionId"],
            json!(expected),
            "decision outcome parity broke for kind '{kind}'"
        );
    }
}

#[test]
fn classifier_buckets_kinds_into_taxonomy() {
    for kind in ["read", "search", "grep", "list", "view", "find"] {
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
    for kind in ["fetch", "think", "switch_mode", "other", "bogus"] {
        let (variant, _) = classify_acp_request_kind(&json!({"toolCall": {"kind": kind}}));
        assert_eq!(variant, AcpRequestKind::Unknown, "expected unknown for {kind}");
    }
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

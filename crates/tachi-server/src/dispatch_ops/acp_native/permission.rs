use serde_json::{json, Value};

/// Typed taxonomy of ACP `session/request_permission` requests.
///
/// Derived from the Agent Client Protocol `ToolKind` enum carried on
/// `params.toolCall.kind` — the structured field the agent sends alongside a
/// permission request (see the recorded shape exercised in `tests.rs`:
/// `{"toolCall": {"kind": "read"|"edit", ...}, "options": [...]}`).
///
/// The prior authorizer keyword-grepped the *entire* serialized request (title,
/// file paths, and option labels included), so a write op whose title merely
/// mentioned "read" could flip to allow — the same fail-open disease as #919.
/// This taxonomy reads only the structured kind, matched by exact equality, and
/// treats anything it does not positively recognize as [`AcpRequestKind::Unknown`]
/// → DENY (fail-closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AcpRequestKind {
    ReadOp,
    WriteOp,
    ExecuteOp,
    Unknown,
}

impl AcpRequestKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            AcpRequestKind::ReadOp => "read_op",
            AcpRequestKind::WriteOp => "write_op",
            AcpRequestKind::ExecuteOp => "execute_op",
            AcpRequestKind::Unknown => "unknown",
        }
    }
}

/// Outcome of authorizing one ACP permission request, carrying enough context
/// for the caller to emit a receipt naming the request kind.
pub(super) struct AcpPermissionDecision {
    pub response: Value,
    pub kind: AcpRequestKind,
    /// The raw `toolCall.kind` string seen (or `<missing>` when absent).
    pub raw_kind: String,
    pub allowed: bool,
    /// True when the deprecated substring heuristic produced this decision.
    pub heuristic: bool,
}

/// Extract the ACP `ToolKind` string from a permission request, if present.
fn tool_kind_str(params: &Value) -> Option<String> {
    params
        .get("toolCall")
        .and_then(|tool_call| tool_call.get("kind").or_else(|| tool_call.get("tool_kind")))
        .or_else(|| params.get("kind"))
        .and_then(Value::as_str)
        .map(|kind| kind.trim().to_ascii_lowercase())
        .filter(|kind| !kind.is_empty())
}

/// Classify an ACP permission request into the typed taxonomy.
///
/// Recognized kinds are matched by exact equality on the structured
/// `toolCall.kind` field — never substring search over the whole request.
/// Read-only ACP `ToolKind`s and their read synonyms map to
/// [`AcpRequestKind::ReadOp`]; mutating kinds map to [`AcpRequestKind::WriteOp`];
/// command execution maps to [`AcpRequestKind::ExecuteOp`]; everything else
/// (a missing kind, the ACP `fetch`/`think`/`switch_mode`/`other` kinds, or any
/// value the client does not recognize) is [`AcpRequestKind::Unknown`].
pub(super) fn classify_acp_request_kind(params: &Value) -> (AcpRequestKind, String) {
    match tool_kind_str(params) {
        Some(kind) => {
            let variant = match kind.as_str() {
                "read" | "search" | "grep" | "list" | "view" | "find" => AcpRequestKind::ReadOp,
                "write" | "edit" | "delete" | "remove" | "create" | "move" => {
                    AcpRequestKind::WriteOp
                }
                "execute" | "exec" | "terminal" | "shell" | "run" => AcpRequestKind::ExecuteOp,
                _ => AcpRequestKind::Unknown,
            };
            (variant, kind)
        }
        None => (AcpRequestKind::Unknown, "<missing>".to_string()),
    }
}

/// Deprecation gate: `TACHI_ACP_PERMISSION_HEURISTIC=legacy` restores the old
/// unsound substring authorizer for one deprecation cycle.
fn legacy_heuristic_enabled() -> bool {
    std::env::var("TACHI_ACP_PERMISSION_HEURISTIC")
        .map(|value| value.trim().eq_ignore_ascii_case("legacy"))
        .unwrap_or(false)
}

/// Authorize one ACP permission request under the given profile label, returning
/// both the JSON-RPC outcome and the receipt context.
pub(super) fn native_permission_decision(
    permission_label: &str,
    params: &Value,
) -> AcpPermissionDecision {
    if legacy_heuristic_enabled() {
        return legacy_permission_decision(permission_label, params);
    }

    let (kind, raw_kind) = classify_acp_request_kind(params);
    // profile -> verdict table. `approve-reads` is the only label the spec layer
    // emits today (see `native_acp_permission_label`); anything else reaches the
    // authorizer only in error and must fail closed. Under `approve-reads` only
    // a `ReadOp` is allowed; write/execute/unknown all DENY with a receipt.
    let allowed = permission_label == "approve-reads" && kind == AcpRequestKind::ReadOp;
    let response = outcome_for(params, allowed);

    AcpPermissionDecision {
        response,
        kind,
        raw_kind,
        allowed,
        heuristic: false,
    }
}

fn legacy_permission_decision(permission_label: &str, params: &Value) -> AcpPermissionDecision {
    tracing::warn!(
        target: "tachi::acp::permission",
        "TACHI_ACP_PERMISSION_HEURISTIC=legacy: using the DEPRECATED substring ACP permission authorizer (unsound, #894 S0). It keyword-greps the entire request and can fail open; unset the env var to use the typed taxonomy."
    );
    // Classify with the taxonomy purely so the receipt still names a request
    // kind, but let the legacy substring rule drive the verdict.
    let (kind, raw_kind) = classify_acp_request_kind(params);
    let allowed = permission_label == "approve-reads" && permission_request_is_read_like(params);
    let response = outcome_for(params, allowed);

    AcpPermissionDecision {
        response,
        kind,
        raw_kind,
        allowed,
        heuristic: true,
    }
}

fn outcome_for(params: &Value, allowed: bool) -> Value {
    select_permission_option(params, allowed)
        .map(selected_outcome)
        .unwrap_or_else(cancelled_outcome)
}

fn selected_outcome(option_id: String) -> Value {
    json!({
        "outcome": {
            "outcome": "selected",
            "optionId": option_id,
        }
    })
}

fn cancelled_outcome() -> Value {
    json!({
        "outcome": {
            "outcome": "cancelled",
        }
    })
}

/// DEPRECATED unsound heuristic, retained one deprecation cycle behind
/// `TACHI_ACP_PERMISSION_HEURISTIC=legacy`. Keyword-greps the entire request;
/// exposed to the sibling test module only for the taxonomy parity tests.
pub(super) fn permission_request_is_read_like(params: &Value) -> bool {
    let haystack = serde_json::to_string(params)
        .unwrap_or_default()
        .to_ascii_lowercase();
    ["read", "search", "grep", "list", "view", "find"]
        .iter()
        .any(|needle| haystack.contains(needle))
        && ![
            "write", "edit", "delete", "remove", "terminal", "shell", "exec", "create",
        ]
        .iter()
        .any(|needle| haystack.contains(needle))
}

fn select_permission_option(params: &Value, approve: bool) -> Option<String> {
    let options = params.get("options").and_then(Value::as_array)?;
    let mut fallback = None;
    for option in options {
        let id = option
            .get("optionId")
            .or_else(|| option.get("id"))
            .or_else(|| option.get("name"))
            .and_then(Value::as_str)?;
        let label = serde_json::to_string(option)
            .unwrap_or_default()
            .to_ascii_lowercase();
        if fallback.is_none() {
            fallback = Some(id.to_string());
        }
        let selected = if approve {
            ["approve", "allow", "accept", "yes", "read"]
                .iter()
                .any(|needle| label.contains(needle))
                && !["deny", "reject", "cancel", "no"]
                    .iter()
                    .any(|needle| label.contains(needle))
        } else {
            ["deny", "reject", "cancel", "no"]
                .iter()
                .any(|needle| label.contains(needle))
        };
        if selected {
            return Some(id.to_string());
        }
    }
    if approve {
        fallback
    } else {
        None
    }
}

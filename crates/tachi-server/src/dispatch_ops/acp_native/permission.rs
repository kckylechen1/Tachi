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

/// Extract the **canonical** ACP `ToolKind` string: `params.toolCall.kind`
/// only. This is the sole field allowed to produce an ALLOW verdict.
fn canonical_tool_kind_str(params: &Value) -> Option<String> {
    params
        .get("toolCall")
        .and_then(|tool_call| tool_call.get("kind"))
        .and_then(Value::as_str)
        .map(|kind| kind.trim().to_ascii_lowercase())
        .filter(|kind| !kind.is_empty())
}

/// Extract every non-canonical "kind-shaped" field we can find (the ACP
/// `toolCall.tool_kind` alias and a stray top-level `kind`). These exist only
/// to (a) enrich the DENY receipt with what a malformed/legacy request
/// claimed, and (b) let a conflicting fallback kind force a DENY even when it
/// would otherwise look read-like — they can NEVER promote a request to
/// ReadOp/allow. See `classify_acp_request_kind` for how they're used.
fn fallback_tool_kind_strs(params: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(tool_call) = params.get("toolCall") {
        if let Some(kind) = tool_call.get("tool_kind").and_then(Value::as_str) {
            let kind = kind.trim().to_ascii_lowercase();
            if !kind.is_empty() {
                out.push(kind);
            }
        }
    }
    if let Some(kind) = params.get("kind").and_then(Value::as_str) {
        let kind = kind.trim().to_ascii_lowercase();
        if !kind.is_empty() {
            out.push(kind);
        }
    }
    out
}

fn bucket_kind(kind: &str) -> AcpRequestKind {
    match kind {
        // ACP spec `ToolKind` read-only values only (#894 hardening: grep/list/
        // view/find were legacy-parity carryover from the substring heuristic,
        // not ACP spec values — they now classify Unknown→DENY, an intentional
        // safety-over-parity behavior change; see commit message).
        "read" | "search" => AcpRequestKind::ReadOp,
        "write" | "edit" | "delete" | "remove" | "create" | "move" => AcpRequestKind::WriteOp,
        "execute" | "exec" | "terminal" | "shell" | "run" => AcpRequestKind::ExecuteOp,
        _ => AcpRequestKind::Unknown,
    }
}

/// Classify an ACP permission request into the typed taxonomy.
///
/// **Only the canonical `toolCall.kind` field may produce
/// [`AcpRequestKind::ReadOp`] (i.e. an ALLOW-capable classification).** A
/// request with no canonical `toolCall.kind` is `Unknown` — full stop — even
/// if a fallback field (`toolCall.tool_kind`, or a stray top-level `kind`)
/// looks read-like; those fields are receipt/deny-enrichment only. If a
/// fallback field disagrees with the canonical kind (or exists when the
/// canonical kind is absent) in a way that would itself classify as
/// write/execute, that also forces `Unknown` so a crafted conflicting request
/// cannot be argued into an allow by a reviewer relying on the raw kind
/// string alone — the returned `raw_kind` always reflects the canonical
/// field (or `<missing>`), so receipts still show what was actually decided.
pub(super) fn classify_acp_request_kind(params: &Value) -> (AcpRequestKind, String) {
    let canonical = canonical_tool_kind_str(params);
    let fallbacks = fallback_tool_kind_strs(params);

    match canonical {
        Some(kind) => {
            let variant = bucket_kind(&kind);
            // A canonical ReadOp classification can still be forced to Unknown
            // if any fallback field disagrees with it — a crafted request that
            // sets a benign toolCall.kind while carrying a conflicting
            // tool_kind/kind must not sail through as an allow.
            let variant = if variant == AcpRequestKind::ReadOp
                && fallbacks.iter().any(|fb| fb != &kind)
            {
                AcpRequestKind::Unknown
            } else {
                variant
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
    let selected = if allowed {
        select_allow_option(params)
    } else {
        // DENY must never be decided by substring-matching option names/ids —
        // a crafted option like {"optionId": "allow-once", "name": "Do not
        // reject", "kind": "allow_once"} would substring-match "reject" in the
        // name and get selected as if it were a deny, when its typed kind says
        // it is in fact an allow option. Select strictly by the ACP typed
        // PermissionOption `kind` field (`reject_once` / `reject_always`).
        select_deny_option(params)
    };
    selected.map(selected_outcome).unwrap_or_else(cancelled_outcome)
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

/// ALLOW-path option selection. Unchanged from the pre-#894-hardening
/// behavior: substring match over the option's serialized name/id, falling
/// back to the first listed option if nothing matches. This path only ever
/// runs when the request was already classified `ReadOp` via the canonical
/// `toolCall.kind`, so a mis-picked *allow* option among several allow-shaped
/// choices is not a fail-open risk the way DENY selection was.
fn select_allow_option(params: &Value) -> Option<String> {
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
        let selected = ["approve", "allow", "accept", "yes", "read"]
            .iter()
            .any(|needle| label.contains(needle))
            && !["deny", "reject", "cancel", "no"]
                .iter()
                .any(|needle| label.contains(needle));
        if selected {
            return Some(id.to_string());
        }
    }
    fallback
}

/// DENY-path option selection. Selects strictly by the ACP typed
/// `PermissionOption.kind` field (`reject_once` / `reject_always`) — **never**
/// by substring-matching the option's `optionId`/`name`. If no option
/// advertises a reject kind, there is no ACP-legal way to positively refuse,
/// so the caller falls back to the `cancelled` outcome, which the ACP spec
/// defines as the client's own refusal to select any option — a legal DENY
/// response that does not require a reject-kind option to exist.
fn select_deny_option(params: &Value) -> Option<String> {
    let options = params.get("options").and_then(Value::as_array)?;
    for option in options {
        let Some(kind) = option.get("kind").and_then(Value::as_str) else {
            continue;
        };
        if matches!(kind, "reject_once" | "reject_always") {
            if let Some(id) = option
                .get("optionId")
                .or_else(|| option.get("id"))
                .and_then(Value::as_str)
            {
                return Some(id.to_string());
            }
        }
    }
    None
}

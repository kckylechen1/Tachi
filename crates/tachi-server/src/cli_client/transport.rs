//! Streamable HTTP MCP transport calls used by CLI and stdio proxy surfaces.

use std::collections::HashMap;
use std::time::Duration;

use http::{HeaderName, HeaderValue};
use rmcp::model::{CallToolRequestParams, ListToolsResult, RawContent};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::ServiceExt;
use serde_json::Value;

use super::detect::DaemonInfo;
use super::tool_map::remap_daemon_tool;
use crate::tools::{
    TASK_CONTROL_TIMEOUT_CAP_SECS, TASK_CONTROL_TIMEOUT_DEFAULT_SECS,
    TASK_WAIT_TIMEOUT_CAP_SECS, TASK_WAIT_TIMEOUT_DEFAULT_SECS,
};

const DAEMON_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Headroom added on top of a caller-supplied poll/control timeout so the
/// outer RPC timeout comfortably outlives the daemon-side wait/control loop
/// (which returns its own terminal payload — e.g. `"status":"timeout"` — as
/// a *successful* response at its own deadline; the RPC timeout is only
/// meant to catch cases where the daemon hangs past that, not to race it).
const LONG_POLL_TIMEOUT_MARGIN: Duration = Duration::from_secs(30);

/// Derive the outer RPC timeout to use for `peer.call_tool(params)`.
///
/// Most tools get the fixed `DAEMON_CALL_TIMEOUT` (60s) baseline. A small
/// whitelist of `tachi_task` long-poll/control actions carry their own
/// caller-supplied timeout in `arguments.timeout_secs` and can legitimately
/// run longer than 60s by design:
///
/// - `action == "wait"` polls up to `timeout_secs`, default
///   [`TASK_WAIT_TIMEOUT_DEFAULT_SECS`] (600s), capped at
///   [`TASK_WAIT_TIMEOUT_CAP_SECS`] (86_400s / 24h) —
///   `tools/task_facade.rs::handle_tachi_task_wait`.
/// - `action == "status"` / `action == "cancel"` drive the acpx
///   control-plane call with the same shape, default
///   [`TASK_CONTROL_TIMEOUT_DEFAULT_SECS`] (30s), capped at
///   [`TASK_CONTROL_TIMEOUT_CAP_SECS`] (300s) —
///   `handle_tachi_task_status` / `handle_tachi_task_cancel`.
///
/// These four constants are shared (`crate::tools::TASK_*`) with the
/// daemon-side handlers rather than mirrored, so the RPC-layer default/cap
/// can't drift out of sync with what the daemon actually applies (see #970,
/// #991, #1028).
///
/// The **invariant**: for every whitelisted action, the derived RPC timeout
/// is `min(requested_or_default, daemon_cap) + LONG_POLL_TIMEOUT_MARGIN` —
/// the daemon-side cap is applied *first*, then the margin is added on top.
/// That ordering means the margin is never eaten by capping (the #970/#1028
/// bug: capping `requested + margin` at a ceiling equal to the daemon's own
/// cap could squeeze the margin to zero right at the boundary, racing the
/// daemon's own clean timeout response), and it holds across the whole
/// parameter domain including the cap boundary itself — there is no longer
/// a separate outer ceiling to keep in sync with the per-action cap.
///
/// `timeout_secs` is coerced with [`tachi_params::opt_u64_from_value`] — the
/// same lenient Null/Number/String-or-number mapping the daemon-side
/// `TachiTaskParams::timeout_secs` field uses via
/// `opt_u64_from_string_or_number` — so a JSON number, a numeric string
/// (`"600"`), `null`, an empty string, or an absent key all resolve to the
/// *same* effective default the daemon will actually apply, instead of the
/// RPC layer silently disagreeing with the daemon about what "no timeout
/// supplied" means.
///
/// A non-numeric garbage string (e.g. `"abc"`) or a negative number also
/// maps to `None` → the action's default here, even though daemon-side
/// `TachiTaskParams` deserialization would *reject* that value outright (a
/// hard parse error before the wait/control loop ever starts). That's
/// intentionally harmless: the whole `tachi_task` call fails fast on the
/// daemon's strict param parse, so no real long-running call is ever
/// entered under that value — the derived RPC timeout here only needs to
/// not undercut/race that fast failure, and it doesn't.
pub(super) fn daemon_call_timeout(params: &CallToolRequestParams) -> Duration {
    if params.name.as_ref() != "tachi_task" {
        return DAEMON_CALL_TIMEOUT;
    }
    let Some(arguments) = params.arguments.as_ref() else {
        return DAEMON_CALL_TIMEOUT;
    };
    let action = arguments.get("action").and_then(Value::as_str).unwrap_or("");
    let (default_secs, cap_secs) = match action {
        "wait" => (TASK_WAIT_TIMEOUT_DEFAULT_SECS, TASK_WAIT_TIMEOUT_CAP_SECS),
        "status" | "cancel" => (
            TASK_CONTROL_TIMEOUT_DEFAULT_SECS,
            TASK_CONTROL_TIMEOUT_CAP_SECS,
        ),
        _ => return DAEMON_CALL_TIMEOUT,
    };

    let requested_secs = arguments
        .get("timeout_secs")
        .and_then(tachi_params::opt_u64_from_value)
        .unwrap_or(default_secs);

    // Cap to the daemon's own effective wait/control ceiling *before*
    // adding the margin — see the invariant in the doc comment above. This
    // is the #1028 fix: the old code added the margin first and only then
    // capped at a ceiling equal to the daemon's own cap, which could shave
    // the margin down to nothing right at the boundary.
    let effective_secs = requested_secs.min(cap_secs);
    Duration::from_secs(effective_secs) + LONG_POLL_TIMEOUT_MARGIN
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonCallError {
    BeforeDispatch(String),
    AfterDispatch(String),
}

impl DaemonCallError {
    pub(super) fn message(&self) -> &str {
        match self {
            Self::BeforeDispatch(message) | Self::AfterDispatch(message) => message,
        }
    }

    pub(crate) fn allows_in_process_fallback(&self) -> bool {
        matches!(self, Self::BeforeDispatch(_))
    }
}

impl std::fmt::Display for DaemonCallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for DaemonCallError {}

/// Call a tool over MCP-streamable-HTTP against a known daemon URL.
/// Returns the first text content block from the tool result.
///
/// `proxy_project` lets a CLI invocation that already knows its named project
/// declare that binding to the daemon by forwarding `X-Tachi-Project` via rmcp's
/// `custom_headers` builder (same mechanism the stdio proxy uses through
/// `call_daemon_tool_raw`). This is required for the CLI named-project
/// forwarding path (`dispatch_cli_tool`): the CLI injects an explicit `project=`
/// arg, and without a bound session the daemon-side C1 guard
/// (`reject_unbound_cross_project_write`) would reject it as an unbound
/// cross-tenant write. Global-only CLI invocations and vault actions pass `None`
/// (no explicit `project=` arg → C1 allows). See `call_daemon_tool_raw` for the
/// header injection details.
pub(crate) async fn call_daemon_tool(
    info: &DaemonInfo,
    tool_name: &str,
    arguments: serde_json::Map<String, Value>,
    proxy_project: Option<&str>,
) -> Result<String, DaemonCallError> {
    let (daemon_tool, daemon_args) = remap_daemon_tool(tool_name, arguments);
    let mut params = CallToolRequestParams::new(daemon_tool.clone());
    if !daemon_args.is_empty() {
        params = params.with_arguments(daemon_args);
    }
    let result = call_daemon_tool_raw(info, params, proxy_project).await?;
    if result.is_error.unwrap_or(false) {
        let err_text =
            first_text_block(&result.content).unwrap_or_else(|| "<no error text>".to_string());
        return Err(DaemonCallError::AfterDispatch(format!(
            "daemon tool '{daemon_tool}' returned error: {err_text}"
        )));
    }

    Ok(first_text_block(&result.content).unwrap_or_else(|| "{}".to_string()))
}

/// Call a daemon tool over Streamable HTTP without remapping the name or
/// collapsing the result to text. stdio proxy mode uses this to stay a pure
/// transport adapter while the daemon remains the semantic owner.
///
/// `proxy_project` lets the stdio proxy declare its bound project identity to
/// the daemon by forwarding `X-Tachi-Project` via rmcp's `custom_headers`
/// builder. The proxy already enforces the binding client-side
/// (`prepare_proxy_tool_call` → `enforce_session_project`), so forwarding it
/// keeps the daemon-side binding consistent with the already-enforced `project=`
/// arg and lets the fail-closed C1 guard (`reject_unbound_cross_project_write`)
/// accept the proxied write instead of rejecting it as unbound. CLI invocations
/// have no proxy project and pass `None`.
pub(crate) async fn call_daemon_tool_raw(
    info: &DaemonInfo,
    params: CallToolRequestParams,
    proxy_project: Option<&str>,
) -> Result<rmcp::model::CallToolResult, DaemonCallError> {
    let tool_name = params.name.as_ref().to_string();
    let mut transport_config = StreamableHttpClientTransportConfig::with_uri(info.url.clone());
    if let Some(project) = proxy_project {
        let mut headers = HashMap::new();
        headers.insert(
            HeaderName::from_static(crate::session_identity::HEADER_PROJECT),
            HeaderValue::from_str(project).map_err(|e| {
                DaemonCallError::BeforeDispatch(format!("invalid proxy project header value: {e}"))
            })?,
        );
        transport_config = transport_config.custom_headers(headers);
    }
    let transport = StreamableHttpClientTransport::from_config(transport_config);
    let client = ServiceExt::serve((), transport).await.map_err(|e| {
        DaemonCallError::BeforeDispatch(format!("daemon handshake failed at {}: {e}", info.url))
    })?;

    let call_timeout = daemon_call_timeout(&params);
    let peer = client.peer().clone();
    let result = tokio::time::timeout(call_timeout, peer.call_tool(params))
        .await
        .map_err(|_| {
            DaemonCallError::AfterDispatch(format!(
                "daemon call '{tool_name}' timed out after {:?}",
                call_timeout
            ))
        })?
        .map_err(|e| {
            DaemonCallError::AfterDispatch(format!("daemon call '{tool_name}' failed: {e}"))
        })?;

    // Dropping the client is enough to close the short-lived CLI HTTP session.
    // Calling `cancel()` here has caused daemon-side lifecycle confusion with
    // rmcp streamable HTTP: a successful forwarded write could be followed by
    // the daemon exiting and leaving a stale pid file.
    drop(client);

    Ok(result)
}

/// List daemon tools over Streamable HTTP. stdio proxy mode uses daemon-side
/// discovery so it does not need to construct a local MemoryServer or open DBs.
pub(crate) async fn list_daemon_tools(
    info: &DaemonInfo,
    params: Option<rmcp::model::PaginatedRequestParams>,
) -> Result<ListToolsResult, DaemonCallError> {
    let transport_config = StreamableHttpClientTransportConfig::with_uri(info.url.clone());
    let transport = StreamableHttpClientTransport::from_config(transport_config);
    let client = ServiceExt::serve((), transport).await.map_err(|e| {
        DaemonCallError::BeforeDispatch(format!("daemon handshake failed at {}: {e}", info.url))
    })?;

    let peer = client.peer().clone();
    let result = tokio::time::timeout(DAEMON_CALL_TIMEOUT, peer.list_tools(params))
        .await
        .map_err(|_| {
            DaemonCallError::AfterDispatch(format!(
                "daemon tools/list timed out after {:?}",
                DAEMON_CALL_TIMEOUT
            ))
        })?
        .map_err(|e| DaemonCallError::AfterDispatch(format!("daemon tools/list failed: {e}")))?;

    drop(client);
    Ok(result)
}

fn first_text_block(blocks: &[rmcp::model::Annotated<RawContent>]) -> Option<String> {
    blocks.iter().find_map(|c| match &c.raw {
        RawContent::Text(t) => Some(t.text.clone()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params_with_args(name: &str, args: serde_json::Map<String, Value>) -> CallToolRequestParams {
        let mut params = CallToolRequestParams::new(name.to_string());
        if !args.is_empty() {
            params = params.with_arguments(args);
        }
        params
    }

    fn wait_params(timeout_secs: Option<Value>) -> CallToolRequestParams {
        action_params("wait", timeout_secs)
    }

    fn action_params(action: &str, timeout_secs: Option<Value>) -> CallToolRequestParams {
        let mut args = serde_json::Map::new();
        args.insert("action".into(), Value::String(action.into()));
        if let Some(v) = timeout_secs {
            args.insert("timeout_secs".into(), v);
        }
        params_with_args("tachi_task", args)
    }

    #[test]
    fn normal_tool_gets_default_timeout() {
        let params = params_with_args("tachi_memory", serde_json::Map::new());
        assert_eq!(daemon_call_timeout(&params), DAEMON_CALL_TIMEOUT);
    }

    #[test]
    fn wait_with_explicit_timeout_gets_timeout_plus_margin() {
        let params = wait_params(Some(json!(600)));
        assert_eq!(
            daemon_call_timeout(&params),
            Duration::from_secs(600) + LONG_POLL_TIMEOUT_MARGIN
        );
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(630));
    }

    #[test]
    fn wait_with_absent_timeout_mirrors_task_facade_default() {
        // handle_tachi_task_wait's own unwrap_or(600) is the daemon-side
        // source of truth for "no timeout_secs supplied" — the RPC layer
        // must not undercut that with a generic 60s fallback, or #970
        // recurs for the common no-argument wait call.
        let params = wait_params(None);
        assert_eq!(
            daemon_call_timeout(&params),
            Duration::from_secs(600) + LONG_POLL_TIMEOUT_MARGIN
        );
    }

    // #1028: this replaces the old `wait_above_ceiling_is_capped` assertion
    // (`daemon_call_timeout == DAEMON_CALL_TIMEOUT_CEILING`, i.e. exactly
    // 86_400s with the margin fully eaten). That was the bug: capping
    // `requested + margin` at a ceiling equal to the daemon's own cap
    // squeezes the margin to zero right at the boundary the daemon itself
    // uses, racing its clean `"status":"timeout"` response instead of
    // comfortably outliving it. The fix caps `requested` at the daemon's
    // cap *first*, then always adds the full margin on top — so a
    // wildly-over-cap request and a request sitting exactly on the cap
    // boundary both land at `cap + margin`, never `cap`.
    #[test]
    fn wait_above_cap_gets_cap_plus_margin_not_truncated() {
        let params = wait_params(Some(json!(999_999)));
        assert_eq!(
            daemon_call_timeout(&params),
            Duration::from_secs(TASK_WAIT_TIMEOUT_CAP_SECS) + LONG_POLL_TIMEOUT_MARGIN
        );
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(86_430));
    }

    #[test]
    fn wait_at_cap_boundary_still_gets_full_margin() {
        // requested == the daemon's own cap exactly (86_400s) is the
        // precise boundary #1028 called out: the old ceiling-after-margin
        // code shaved the margin to 0 here. min(86_400, 86_400) + 30 must
        // be 86_430, not 86_400.
        let params = wait_params(Some(json!(TASK_WAIT_TIMEOUT_CAP_SECS)));
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(86_430));
    }

    #[test]
    fn status_with_explicit_timeout_gets_timeout_plus_margin() {
        let params = action_params("status", Some(json!(300)));
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(330));
    }

    #[test]
    fn status_above_cap_is_capped_before_margin() {
        // requested (301) > daemon cap (300) for status/cancel: the daemon
        // itself clamps to 300 (`unwrap_or(30).min(300)` in
        // `handle_tachi_task_status`), so the RPC layer must derive from
        // the *clamped* 300, not the raw 301 — otherwise it'd still be
        // correct by accident here, but the point is the cap, not the
        // requested value, drives the derived timeout once over cap.
        let params = action_params("status", Some(json!(301)));
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(330));
    }

    #[test]
    fn status_with_absent_timeout_mirrors_task_facade_default() {
        let params = action_params("status", None);
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(60));
    }

    #[test]
    fn cancel_with_explicit_timeout_gets_timeout_plus_margin() {
        let params = action_params("cancel", Some(json!(300)));
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(330));
    }

    #[test]
    fn cancel_above_cap_is_capped_before_margin() {
        let params = action_params("cancel", Some(json!(999_999)));
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(330));
    }

    #[test]
    fn cancel_with_absent_timeout_mirrors_task_facade_default() {
        let params = action_params("cancel", None);
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(60));
    }

    #[test]
    fn tachi_task_non_wait_action_gets_default_timeout() {
        let mut args = serde_json::Map::new();
        args.insert("action".into(), Value::String("board".into()));
        let params = params_with_args("tachi_task", args);
        assert_eq!(daemon_call_timeout(&params), DAEMON_CALL_TIMEOUT);
    }

    // --- Review-fix (#970 follow-up): string/null timeout_secs must not
    // truncate the wait. `opt_u64_from_value` is the same lenient
    // Null/Number/String-or-number coercion the daemon-side
    // `TachiTaskParams::timeout_secs` field uses, so a numeric string, an
    // explicit null, an absent key, and an empty string must all derive
    // the *same* timeout the daemon will actually apply — the wait
    // default (600s) + margin (30s) = 630s — not the generic 60s
    // baseline. This replaces the old (wrong) assertion that null → 60s,
    // which locked in the truncation bug this test module now guards
    // against.

    #[test]
    fn wait_with_numeric_string_timeout_gets_timeout_plus_margin() {
        let params = wait_params(Some(json!("600")));
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(630));
    }

    #[test]
    fn wait_with_null_timeout_mirrors_wait_default() {
        let params = wait_params(Some(json!(null)));
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(630));
    }

    #[test]
    fn wait_with_empty_string_timeout_mirrors_wait_default() {
        let params = wait_params(Some(json!("")));
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(630));
    }

    #[test]
    fn wait_with_garbage_string_timeout_falls_back_to_wait_default_not_60s() {
        // A non-numeric string ("abc") coerces to `None` here, same as
        // null/absent, and derives the wait default (630s) rather than the
        // generic 60s baseline. This is deliberately harmless even though
        // it looks generous: daemon-side `TachiTaskParams` deserialization
        // uses the *strict* `opt_u64_from_string_or_number` deserializer,
        // which hard-errors on a non-numeric string — the whole
        // `tachi_task` call fails fast on the daemon's param parse before
        // any wait loop starts, so a real 600s+ wait is never actually
        // entered under this value. Garbage->None->default is chosen for
        // consistency with null/absent/empty-string rather than adding a
        // separate garbage->60s special case that would just be a second
        // codepath to keep in sync for no behavioral benefit (the call
        // errors out well within either 60s or 630s regardless).
        let params = wait_params(Some(json!("not-a-number")));
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(630));
    }

    #[test]
    fn wait_with_negative_number_timeout_falls_back_to_wait_default() {
        // Same reasoning as the garbage-string case: a negative JSON
        // number isn't representable as u64, coerces to `None`, and the
        // daemon-side strict deserializer would hard-error on it too (the
        // call fails fast, no real long wait is ever entered).
        let params = wait_params(Some(json!(-5)));
        assert_eq!(daemon_call_timeout(&params), Duration::from_secs(630));
    }

    #[test]
    fn missing_arguments_map_entirely_gets_default_timeout() {
        // tachi_task with action=wait but no arguments map at all (not just
        // a missing key) must still fail safe to the 60s default — there is
        // no `arguments` map to read `timeout_secs` from at all, which is a
        // distinct case from a present-but-absent/null `timeout_secs` key
        // (those go through the wait-default path above).
        let params = CallToolRequestParams::new("tachi_task".to_string());
        assert_eq!(daemon_call_timeout(&params), DAEMON_CALL_TIMEOUT);
    }
}

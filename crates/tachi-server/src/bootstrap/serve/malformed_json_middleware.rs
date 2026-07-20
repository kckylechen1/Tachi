//! Malformed-MCP-JSON response normalization middleware.
//!
//! PARITY CONTRACT: this file must stay byte-identical to its twin —
//! `crates/tachi-server/src/bootstrap/serve/malformed_json_middleware.rs` in
//! tachi-server, `crates/portable-server/src/malformed_json_middleware.rs` in
//! portable-server. tachi-server's
//! `malformed_json_middleware_stays_in_parity_with_portable_server` test
//! (`crates/tachi-server/src/bootstrap/serve/daemon.rs`) enforces this by
//! `include_str!`-comparing the two files byte-for-byte at test time — edit
//! one, edit its twin too, or the test fails loudly.
//!
//! Why two copies instead of one shared crate: portable-server is
//! deliberately zero-dependency on tachi-server, by construction (see
//! `crates/portable-server/Cargo.toml`'s module doc), and there is no
//! existing crate both binaries already depend on that this ~30-line axum
//! middleware could live in without adding a new dependency edge (tachi
//! #1308 follow-up). Enforced duplication was the ruled-on tradeoff instead
//! of introducing that edge or a new crate for it.

use axum::response::IntoResponse;

/// axum middleware layered onto the `/mcp` route: rmcp's streamable-HTTP
/// server replies with a plain-text 415 Unsupported Media Type when the
/// POST body isn't valid JSON — its Content-Type/body sniffing rejects
/// malformed bytes before it can build a JSON-RPC envelope to report a
/// parse error through. For an MCP client, a raw parse failure on a
/// request that already declared `Content-Type: application/json` is
/// better modeled as a JSON-RPC `-32700 Parse error` than an opaque
/// HTTP-level 415. A client sending a genuinely unsupported content type
/// (e.g. `text/plain`) still gets the real 415, unmodified.
pub(crate) async fn normalize_malformed_mcp_json_response(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let is_mcp_post = request.method() == axum::http::Method::POST
        && matches!(request.uri().path(), "/mcp" | "/mcp/");
    let is_json = request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"));
    let response = next.run(request).await;
    if is_mcp_post && is_json && response.status() == axum::http::StatusCode::UNSUPPORTED_MEDIA_TYPE
    {
        return parse_error_response();
    }
    response
}

fn parse_error_response() -> axum::response::Response {
    (
        axum::http::StatusCode::BAD_REQUEST,
        axum::Json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": { "code": -32700, "message": "Parse error" }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn malformed_mcp_json_response_is_structured_parse_error() {
        let response = parse_error_response();
        assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "application/json"
        );
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("parse error response body");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON-RPC body");
        assert_eq!(body["jsonrpc"], "2.0");
        assert!(body["id"].is_null());
        assert_eq!(body["error"]["code"], -32700);
        assert_eq!(body["error"]["message"], "Parse error");
    }
}

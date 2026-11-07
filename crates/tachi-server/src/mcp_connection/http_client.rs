//! Bounded HTTP bodies around the SDK transport. Session negotiation, tool
//! serialization, pagination and OAuth remain owned by rmcp.
use super::REMOTE_MCP_MAX_RESPONSE_BYTES;
use futures::StreamExt;
use reqwest::header::{HeaderName, HeaderValue};
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::common::client_side_sse::BoxedSseResponse;
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, HttpStatusError, InsufficientScopeError, StreamableHttpClient,
    StreamableHttpError, StreamableHttpPostResponse,
};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone)]
pub(super) struct McpHttpClient(pub reqwest::Client);

type HttpError = StreamableHttpError<reqwest::Error>;

fn invalid_response(message: &'static str) -> HttpError {
    StreamableHttpError::UnexpectedServerResponse(message.into())
}

fn without_url(error: HttpError) -> HttpError {
    match error {
        StreamableHttpError::Client(error) => StreamableHttpError::Client(error.without_url()),
        error => error,
    }
}

impl StreamableHttpClient for McpHttpClient {
    type Error = reqwest::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        let mut request = self.0.post(uri.as_ref()).header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        );
        if let Some(token) = auth_token {
            request = request.bearer_auth(token);
        }
        for (name, value) in custom_headers {
            if matches!(name.as_str(), "accept" | "mcp-session-id" | "last-event-id") {
                return Err(StreamableHttpError::ReservedHeaderConflict(
                    name.to_string(),
                ));
            }
            request = request.header(name, value);
        }
        let had_session = session_id.is_some();
        if let Some(session_id) = session_id {
            request = request.header("mcp-session-id", session_id.as_ref());
        }
        let response = request
            .json(&message)
            .send()
            .await
            .map_err(|error| StreamableHttpError::Client(error.without_url()))?;
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403) {
            if let Some(challenge) = response.headers().get(reqwest::header::WWW_AUTHENTICATE) {
                let challenge = challenge
                    .to_str()
                    .map_err(|_| invalid_response("Invalid authentication challenge"))?
                    .to_string();
                return Err(if status.as_u16() == 401 {
                    StreamableHttpError::AuthRequired(AuthRequiredError::new(challenge))
                } else {
                    // The owner can authorize a new scope through mcp login.
                    StreamableHttpError::InsufficientScope(InsufficientScopeError::new(
                        challenge, None,
                    ))
                });
            }
            return Err(HttpStatusError::new(status.as_u16(), None).into());
        }
        if matches!(status.as_u16(), 202 | 204) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status.as_u16() == 404 && had_session {
            return Err(StreamableHttpError::SessionExpired);
        }
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if status.as_u16() == 429 {
            return Err(HttpStatusError::new(status.as_u16(), retry_after).into());
        }
        if response
            .content_length()
            .is_some_and(|len| len > REMOTE_MCP_MAX_RESPONSE_BYTES as u64)
        {
            return Err(invalid_response("Remote MCP body exceeds 1048576 bytes"));
        }
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let notification = !matches!(message, ClientJsonRpcMessage::Request(_));
        if status.is_success() && notification && response.content_length() == Some(0) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status.is_success() && content_type.starts_with("text/event-stream") {
            // Keep the previous per-response bound before SSE parsing allocates.
            let stream = response
                .bytes_stream()
                .scan((0usize, false), |(size, failed), item| {
                    let next = if *failed {
                        None
                    } else {
                        Some(match item {
                            Ok(bytes)
                                if bytes.len()
                                    <= REMOTE_MCP_MAX_RESPONSE_BYTES.saturating_sub(*size) =>
                            {
                                *size += bytes.len();
                                Ok(bytes)
                            }
                            Ok(_) => {
                                *failed = true;
                                Err(std::io::Error::other(
                                    "Remote MCP body exceeds 1048576 bytes",
                                ))
                            }
                            Err(_) => {
                                *failed = true;
                                Err(std::io::Error::other("Remote MCP body read failed"))
                            }
                        })
                    };
                    std::future::ready(next)
                });
            return Ok(StreamableHttpPostResponse::Sse(
                sse_stream::SseStream::from_bytes_stream(stream).boxed(),
                session,
            ));
        }
        let body = super::remote::read_remote_mcp_body(response, "remote MCP")
            .await
            .map_err(|_| {
                invalid_response("Remote MCP body read failed or exceeded 1048576 bytes")
            })?;
        let parsed = serde_json::from_str::<ServerJsonRpcMessage>(&body);
        if !status.is_success() {
            return match parsed {
                Ok(error @ ServerJsonRpcMessage::Error(_)) => {
                    Ok(StreamableHttpPostResponse::Json(error, session))
                }
                _ => Err(HttpStatusError::new(status.as_u16(), retry_after).into()),
            };
        }
        if !content_type.starts_with("application/json") {
            return Err(invalid_response("Unexpected remote MCP content type"));
        }
        match parsed {
            Ok(message) => Ok(StreamableHttpPostResponse::Json(message, session)),
            Err(_) if notification => Ok(StreamableHttpPostResponse::Accepted),
            Err(_) => Err(invalid_response("Invalid remote MCP JSON-RPC response")),
        }
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        self.0
            .delete_session(uri, session_id, auth_token, custom_headers)
            .await
            .map_err(without_url)
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_token: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxedSseResponse, HttpError> {
        self.0
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_token,
                custom_headers,
                REMOTE_MCP_MAX_RESPONSE_BYTES,
            )
            .await
            .map_err(without_url)
    }
}

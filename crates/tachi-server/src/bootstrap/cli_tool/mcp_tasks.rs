//! One-shot, loopback-only MCP Tasks client. It owns no store or worker lifecycle.

use reqwest::header::{HeaderValue, ACCEPT, CONTENT_TYPE};
use serde_json::{json, Value};
use std::error::Error;
use std::time::Duration;
use tachi_bootstrap::cli::{McpTaskAction, McpTaskArgs};

const PROTOCOL_VERSION: &str = "2026-07-28";
const TASK_EXTENSION: &str = "io.modelcontextprotocol/tasks";
const CLIENT_NAME: &str = "tachi-cli";
const PROFILE: &str = "coordinate";
const REQUEST_ID: u64 = 1;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_INPUT_BYTES: usize = 64 * 1024;

pub(crate) async fn run_mcp_task_command(args: &McpTaskArgs) -> Result<(), Box<dyn Error>> {
    if !cfg!(any(unix, windows)) {
        return Err(
            "MCP result tasks require a supported Unix or Windows NTFS loopback daemon".into(),
        );
    }
    let (status, envelope) = request_task(args).await?;
    super::print_pretty_json(&envelope)?;
    if let Some(error) = envelope.get("error") {
        return Err(format!(
            "MCP task request returned a JSON-RPC error (code {}); response printed above",
            error["code"]
        )
        .into());
    }
    if !status.is_success() {
        return Err(
            format!("MCP task request returned HTTP {status}; response printed above").into(),
        );
    }
    Ok(())
}

fn prepare_request(
    args: &McpTaskArgs,
) -> Result<(reqwest::Client, reqwest::Request), Box<dyn Error>> {
    let project = identity_header(&args.project, "project")?;
    let identity = identity_header(&args.agent_identity, "agent identity")?;
    if args.port == 0 {
        return Err("MCP task daemon port must be nonzero".into());
    }
    let (method, name, mut params) = match &args.action {
        McpTaskAction::Result { dispatch_id } => {
            if !crate::dispatch_ops::is_valid_dispatch_id(dispatch_id) {
                return Err("MCP task result requires a valid dispatch identifier".into());
            }
            (
                "tools/call",
                "tachi_staff".to_string(),
                json!({
                    "name": "tachi_staff",
                    "arguments": {"action": "result", "dispatch_id": dispatch_id, "format": "json"}
                }),
            )
        }
        McpTaskAction::Get { task_id } => {
            ("tasks/get", task_id.clone(), json!({"taskId": task_id}))
        }
        McpTaskAction::Cancel { task_id } => {
            ("tasks/cancel", task_id.clone(), json!({"taskId": task_id}))
        }
        McpTaskAction::Update {
            task_id,
            input_responses,
        } => {
            if input_responses.len() > MAX_INPUT_BYTES {
                return Err("MCP task input responses exceed the 64 KiB limit".into());
            }
            let input: Value = serde_json::from_str(input_responses)
                .map_err(|_| "MCP task input responses must be a valid JSON object")?;
            if !input.is_object() {
                return Err("MCP task input responses must be a JSON object".into());
            }
            (
                "tasks/update",
                task_id.clone(),
                json!({"taskId": task_id, "inputResponses": input}),
            )
        }
    };
    let routing_name = identity_header(&name, "routing name")?;
    params["_meta"] = json!({
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientCapabilities": {
            "elicitation": {"form": {}},
            "extensions": {TASK_EXTENSION: {}}
        },
        "io.modelcontextprotocol/clientInfo": {
            "name": CLIENT_NAME,
            "version": env!("CARGO_PKG_VERSION")
        },
        "tachiProject": args.project,
        "tachiAgentIdentity": args.agent_identity,
        "tachiProfile": PROFILE,
        "tachiClient": CLIENT_NAME
    });
    // This client can run before bootstrap installs reqwest's TLS provider.
    crate::ensure_tls_provider();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(30))
        .build()?;
    let request = client
        .post(format!("http://127.0.0.1:{}/mcp", args.port))
        .header(ACCEPT, "application/json, text/event-stream")
        .header("Mcp-Protocol-Version", PROTOCOL_VERSION)
        .header("Mcp-Method", method)
        .header("Mcp-Name", routing_name)
        .header("X-Tachi-Project", project)
        .header("X-Tachi-Agent-Identity", identity)
        .header("X-Tachi-Profile", PROFILE)
        .header("X-Tachi-Client", CLIENT_NAME)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": REQUEST_ID,
            "method": method,
            "params": params
        }))
        .build()?;
    Ok((client, request))
}

fn identity_header(value: &str, field: &str) -> Result<HeaderValue, Box<dyn Error>> {
    if value.trim().is_empty() || value != value.trim() || value.len() > 1024 {
        return Err(
            format!("MCP task {field} must be nonempty, unpadded, and at most 1024 bytes").into(),
        );
    }
    HeaderValue::from_str(value).map_err(|_| format!("invalid MCP task {field} header").into())
}

async fn request_task(args: &McpTaskArgs) -> Result<(reqwest::StatusCode, Value), Box<dyn Error>> {
    let (client, request) = prepare_request(args)?;
    // Post-dispatch failures are ambiguous, including result-handle creation.
    // A one-shot CLI never replays the request automatically.
    let mut response = client.execute(request).await?;
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err("MCP task response exceeds the 1 MiB limit".into());
    }
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .unwrap_or("")
        .trim();
    let is_sse = content_type.eq_ignore_ascii_case("text/event-stream");
    if !is_sse && !content_type.eq_ignore_ascii_case("application/json") {
        return Err(
            format!("MCP task response has an unsupported content type (HTTP {status})").into(),
        );
    }
    let mut total_bytes = 0usize;
    let mut body = Vec::new();
    let mut events = SseResponse::default();
    while let Some(chunk) = response.chunk().await? {
        total_bytes = total_bytes.saturating_add(chunk.len());
        if total_bytes > MAX_RESPONSE_BYTES {
            return Err("MCP task response exceeds the 1 MiB limit".into());
        }
        if is_sse {
            if let Some(envelope) = events.push(&chunk)? {
                return Ok((status, envelope));
            }
        } else {
            body.extend_from_slice(&chunk);
        }
    }
    if is_sse {
        return Err("MCP task event stream ended without a matching response".into());
    }
    let envelope: Value = serde_json::from_slice(&body)
        .map_err(|_| "MCP task response is not a valid JSON envelope")?;
    matching_response(envelope)?
        .map(|envelope| (status, envelope))
        .ok_or_else(|| "MCP task response has no matching request id".into())
}

fn matching_response(envelope: Value) -> Result<Option<Value>, Box<dyn Error>> {
    let object = envelope
        .as_object()
        .ok_or("MCP task response must be a JSON-RPC object")?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("MCP task response has an invalid JSON-RPC version".into());
    }
    if object.get("id") != Some(&json!(REQUEST_ID)) {
        return Ok(None);
    }
    if object.contains_key("method")
        || object.contains_key("result") == object.contains_key("error")
    {
        return Err("MCP task response must contain exactly one result or error".into());
    }
    if let Some(error) = object.get("error") {
        if !error.is_object()
            || error.get("code").and_then(Value::as_i64).is_none()
            || error.get("message").and_then(Value::as_str).is_none()
        {
            return Err("MCP task response has an invalid JSON-RPC error".into());
        }
    } else if !object["result"].is_object() {
        return Err("MCP task response result must be an object".into());
    }
    Ok(Some(envelope))
}

#[derive(Default)]
struct SseResponse {
    line: Vec<u8>,
    data: Vec<u8>,
    skip_lf: bool,
    started: bool,
}

impl SseResponse {
    fn push(&mut self, chunk: &[u8]) -> Result<Option<Value>, Box<dyn Error>> {
        for &byte in chunk {
            if byte == b'\n' && self.skip_lf {
                self.skip_lf = false;
                continue;
            }
            self.skip_lf = byte == b'\r';
            if byte == b'\n' || byte == b'\r' {
                if let Some(envelope) = self.finish_line()? {
                    return Ok(Some(envelope));
                }
            } else {
                self.line.push(byte);
            }
        }
        Ok(None)
    }

    fn finish_line(&mut self) -> Result<Option<Value>, Box<dyn Error>> {
        let bytes = std::mem::take(&mut self.line);
        let mut line =
            std::str::from_utf8(&bytes).map_err(|_| "MCP task event stream is not valid UTF-8")?;
        if !self.started {
            line = line.strip_prefix('\u{feff}').unwrap_or(line);
            self.started = true;
        }
        if line.is_empty() {
            if self.data.is_empty() {
                return Ok(None);
            }
            let data = std::mem::take(&mut self.data);
            let envelope = serde_json::from_slice(&data)
                .map_err(|_| "MCP task event is not a valid JSON envelope")?;
            return matching_response(envelope);
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        if field == "data" {
            self.data
                .extend_from_slice(value.strip_prefix(' ').unwrap_or(value).as_bytes());
            self.data.push(b'\n');
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(action: McpTaskAction) -> McpTaskArgs {
        McpTaskArgs {
            project: "project-12345678".to_string(),
            agent_identity: "agent-local".to_string(),
            port: 6919,
            action,
        }
    }

    #[test]
    fn request_preparation_initializes_tls_in_fresh_process() {
        const CHILD: &str = "TACHI_MCP_TASK_TLS_TEST_CHILD";
        const READY: &str = "mcp-task-client-ready-without-bootstrap";
        if std::env::var_os(CHILD).is_some() {
            let (_, request) = prepare_request(&args(McpTaskAction::Get {
                task_id: "task-1".into(),
            }))
            .expect("production request preparation installs TLS without bootstrap");
            assert_eq!(request.url().as_str(), "http://127.0.0.1:6919/mcp");
            println!("{READY}");
            return;
        }
        // Process isolation prevents other tests' global provider from masking
        // a missing install in the production client path.
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "bootstrap::cli_tool::hub::mcp_tasks::tests::request_preparation_initializes_tls_in_fresh_process",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .expect("spawn isolated request preparation");
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains(READY),
            "fresh-process request preparation failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn requests_bind_modern_capability_identity_and_routing_without_execution_fields() {
        let cases = [
            (
                McpTaskAction::Result {
                    dispatch_id: "20261030T010101Z-custom-deadbeef".into(),
                },
                "tools/call",
                "tachi_staff",
            ),
            (
                McpTaskAction::Get {
                    task_id: "result-task".into(),
                },
                "tasks/get",
                "result-task",
            ),
            (
                McpTaskAction::Cancel {
                    task_id: "result-task".into(),
                },
                "tasks/cancel",
                "result-task",
            ),
            (
                McpTaskAction::Update {
                    task_id: "result-task".into(),
                    input_responses: r#"{"input-1":{"result":{"value":"answer"}}}"#.into(),
                },
                "tasks/update",
                "result-task",
            ),
        ];
        for (action, method, name) in cases {
            let (_, request) = prepare_request(&args(action)).expect("prepare request");
            assert_eq!(request.url().as_str(), "http://127.0.0.1:6919/mcp");
            assert_eq!(request.headers()["Mcp-Method"], method);
            assert_eq!(request.headers()["Mcp-Name"], name);
            assert_eq!(request.headers()["Mcp-Protocol-Version"], PROTOCOL_VERSION);
            assert!(!request.headers().contains_key("Mcp-Session-Id"));
            let body: Value = serde_json::from_slice(
                request
                    .body()
                    .and_then(|body| body.as_bytes())
                    .expect("JSON body"),
            )
            .expect("decode request");
            assert_eq!(body["method"], method);
            let meta = &body["params"]["_meta"];
            assert_eq!(
                meta["io.modelcontextprotocol/protocolVersion"],
                PROTOCOL_VERSION
            );
            assert_eq!(
                meta["io.modelcontextprotocol/clientCapabilities"],
                json!({"extensions": {TASK_EXTENSION: {}}})
            );
            for (header, key) in [
                ("X-Tachi-Project", "tachiProject"),
                ("X-Tachi-Agent-Identity", "tachiAgentIdentity"),
                ("X-Tachi-Profile", "tachiProfile"),
                ("X-Tachi-Client", "tachiClient"),
            ] {
                assert_eq!(meta[key].as_str(), request.headers()[header].to_str().ok());
            }
            if method == "tools/call" {
                assert_eq!(
                    body["params"]["arguments"],
                    json!({"action":"result", "dispatch_id":"20261030T010101Z-custom-deadbeef", "format":"json"})
                );
            } else {
                assert_eq!(body["params"]["taskId"], name);
                assert!(body["params"].get("expectedRevision").is_none());
                assert!(body["params"].get("expected_status_revision").is_none());
            }
        }
    }

    #[test]
    fn request_admission_rejects_header_injection_invalid_ids_and_unbounded_input() {
        let mut request = args(McpTaskAction::Get {
            task_id: "task".into(),
        });
        request.project = "project\r\nX-Tachi-Profile: admin".into();
        assert!(prepare_request(&request).is_err());
        request.project = " project ".into();
        assert!(prepare_request(&request).is_err());
        request.project = "project".into();
        request.agent_identity.clear();
        assert!(prepare_request(&request).is_err());
        assert!(prepare_request(&args(McpTaskAction::Result {
            dispatch_id: "../secret".into()
        }))
        .is_err());
        for input in [
            "[]".to_string(),
            "broken".to_string(),
            " ".repeat(MAX_INPUT_BYTES + 1),
        ] {
            assert!(prepare_request(&args(McpTaskAction::Update {
                task_id: "task".into(),
                input_responses: input,
            }))
            .is_err());
        }
    }

    #[test]
    fn sse_decodes_chunked_multiline_response_and_ignores_other_ids() {
        let expected = json!({"jsonrpc":"2.0", "id":1, "result":{"resultType":"complete", "status":"completed", "result":{"content":[{"type":"text","text":"完整结果"}]}}});
        let body = format!(
            "\u{feff}: keepalive\r\n\r\ndata: {{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{{}}}}\r\n\r\nevent: message\r\ndata: {{\"jsonrpc\":\"2.0\",\"id\":1,\r\ndata: \"result\":{}}}\r\n\r\n",
            expected["result"]
        );
        for chunk_size in 1..=body.len() {
            let mut decoder = SseResponse::default();
            let mut actual = None;
            for chunk in body.as_bytes().chunks(chunk_size) {
                if let Some(envelope) = decoder.push(chunk).expect("valid stream") {
                    actual = Some(envelope);
                    break;
                }
            }
            assert_eq!(actual, Some(expected.clone()), "chunk size {chunk_size}");
        }
    }

    #[test]
    fn protocol_errors_are_preserved_and_ambiguous_envelopes_are_rejected() {
        let error = json!({"jsonrpc":"2.0", "id":1, "error":{"code":-32021,"message":"missing capability","data":{"requiredCapabilities":{"extensions":{TASK_EXTENSION:{}}}}}});
        assert_eq!(matching_response(error.clone()).unwrap(), Some(error));
        for invalid in [
            json!({"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-32603,"message":"error"}}),
            json!({"jsonrpc":"2.0","id":1,"error":{"code":"-32603","message":"error"}}),
            json!({"jsonrpc":"1.0","id":1,"result":{}}),
            json!({"jsonrpc":"2.0","id":1,"result":null}),
        ] {
            assert!(matching_response(invalid).is_err());
        }
    }

    #[tokio::test]
    async fn http_client_returns_the_matching_sse_response_before_stream_close() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let expected = json!({"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","taskId":"task-1","status":"working"}});
        let event = format!("data: {expected}\n\n");
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                assert!(read > 0, "request ended before headers");
                bytes.extend_from_slice(&buffer[..read]);
                assert!(bytes.len() < 16 * 1024);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let headers = std::str::from_utf8(&bytes).unwrap().to_ascii_lowercase();
            assert!(headers.starts_with("post /mcp http/1.1\r\n"));
            assert!(headers.contains("mcp-method: tasks/get\r\n"));
            assert!(headers.contains("mcp-name: task-1\r\n"));
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
            stream
                .write_all(format!("{:x}\r\n{event}\r\n", event.len()).as_bytes())
                .await
                .unwrap();
            stream.flush().await.unwrap();
            // Keep the HTTP body open until the client has returned its envelope.
            let _ = released.await;
        });
        let mut input = args(McpTaskAction::Get {
            task_id: "task-1".into(),
        });
        input.port = port;
        let response = tokio::time::timeout(Duration::from_secs(3), request_task(&input)).await;
        let _ = release.send(());
        server.await.expect("fixture server joined");
        let (status, envelope) = response
            .expect("matching response before EOF")
            .expect("HTTP task response");
        assert_eq!(status, reqwest::StatusCode::OK);
        assert_eq!(envelope, expected);
    }
}

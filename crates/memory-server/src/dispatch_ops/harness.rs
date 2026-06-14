use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

const HARNESS_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const HARNESS_PROBE_BODY_LIMIT: usize = 1024 * 1024;

struct HttpProbeResponse {
    status_code: u16,
    body: Vec<u8>,
    truncated: bool,
}

fn unsupported_probe(reason: &str) -> Value {
    json!({
        "reachable": null,
        "attach_ready": false,
        "probe": "unsupported",
        "evidence_strength": "none",
        "readiness": "unsupported",
        "layers": {
            "tcp_reachable": "not_run",
            "http_health": "not_run",
            "server_auth": "not_run",
            "opencode_api_version": "not_run",
            "session_create_smoke": "not_run",
            "model_available": "not_run",
            "credential_ready": "not_run",
        },
        "reason": reason,
    })
}

fn tcp_only_status(port: u16, reachable: bool, reason: Option<String>) -> Value {
    let mut status = json!({
        "reachable": reachable,
        "attach_ready": false,
        "probe": "tcp",
        "evidence_strength": "weak",
        "readiness": if reachable { "tcp_only" } else { "unreachable" },
        "port": port,
        "layers": {
            "tcp_reachable": if reachable { "passed" } else { "failed" },
            "http_health": if reachable { "failed" } else { "not_run" },
            "server_auth": "not_run",
            "opencode_api_version": "not_run",
            "session_create_smoke": "not_run",
            "model_available": "not_run",
            "credential_ready": "not_run",
        },
        "warning": "TCP reachability only; OpenCode API version, session creation, model availability, and credentials were not verified",
    });
    if let Some(reason) = reason {
        status["error"] = json!(reason);
    }
    status
}

fn parse_local_http_port(url: &str) -> Option<u16> {
    if !url.starts_with("http://127.0.0.1:") && !url.starts_with("http://localhost:") {
        return None;
    }
    url.split(':')
        .nth(2)
        .and_then(|rest| rest.split('/').next())
        .and_then(|raw| raw.parse::<u16>().ok())
}

fn opencode_basic_auth_header() -> Option<String> {
    let password = std::env::var("OPENCODE_SERVER_PASSWORD")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())?;
    let username = std::env::var("OPENCODE_SERVER_USERNAME")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "opencode".to_string());
    Some(format!(
        "Authorization: Basic {}\r\n",
        B64.encode(format!("{username}:{password}"))
    ))
}

fn read_http_response(
    mut stream: TcpStream,
    port: u16,
    path: &str,
) -> Result<HttpProbeResponse, String> {
    stream
        .set_read_timeout(Some(HARNESS_PROBE_TIMEOUT))
        .map_err(|e| format!("set read timeout: {e}"))?;
    stream
        .set_write_timeout(Some(HARNESS_PROBE_TIMEOUT))
        .map_err(|e| format!("set write timeout: {e}"))?;
    let auth_header = opencode_basic_auth_header().unwrap_or_default();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n{auth_header}Accept: application/json,text/plain,*/*\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write HTTP probe {path}: {e}"))?;

    let mut raw = Vec::new();
    let mut truncated = false;
    let mut buf = [0_u8; 8192];
    loop {
        let n = stream
            .read(&mut buf)
            .map_err(|e| format!("read HTTP probe {path}: {e}"))?;
        if n == 0 {
            break;
        }
        let remaining = HARNESS_PROBE_BODY_LIMIT.saturating_sub(raw.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        let keep = remaining.min(n);
        raw.extend_from_slice(&buf[..keep]);
        if keep < n {
            truncated = true;
            break;
        }
    }
    if raw.is_empty() {
        return Err("empty HTTP probe response".to_string());
    }
    let header_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "HTTP probe response missing header terminator".to_string())?;
    let headers = String::from_utf8_lossy(&raw[..header_end]);
    let status_line = headers.lines().next().unwrap_or_default();
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|raw| raw.parse::<u16>().ok())
        .ok_or_else(|| format!("invalid HTTP probe status line: {status_line}"))?;
    let body = raw[(header_end + 4)..].to_vec();
    Ok(HttpProbeResponse {
        status_code,
        body,
        truncated,
    })
}

fn http_get_path(port: u16, path: &str) -> Result<HttpProbeResponse, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let stream = TcpStream::connect_timeout(&addr, HARNESS_PROBE_TIMEOUT)
        .map_err(|e| format!("connect HTTP probe {path}: {e}"))?;
    read_http_response(stream, port, path)
}

fn has_openapi_route(doc: &Value, path: &str, method: &str) -> bool {
    doc.get("paths")
        .and_then(Value::as_object)
        .and_then(|paths| paths.get(path))
        .and_then(Value::as_object)
        .and_then(|methods| methods.get(method))
        .is_some()
}

fn probe_opencode_doc(port: u16) -> Result<Value, String> {
    let doc = http_get_path(port, "/doc")?;
    if !(200..300).contains(&doc.status_code) {
        return Err(format!("OpenCode /doc returned HTTP {}", doc.status_code));
    }
    if doc.truncated {
        return Err(format!(
            "OpenCode /doc exceeded {} byte probe limit",
            HARNESS_PROBE_BODY_LIMIT
        ));
    }
    let doc = serde_json::from_slice::<Value>(&doc.body)
        .map_err(|e| format!("parse OpenCode /doc schema: {e}"))?;
    let title = doc
        .pointer("/info/title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !title.eq_ignore_ascii_case("opencode") {
        return Err("OpenCode /doc schema did not identify opencode".to_string());
    }
    let version = doc
        .pointer("/info/version")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let api_session_create = has_openapi_route(&doc, "/api/session", "post")
        || has_openapi_route(&doc, "/session", "post");
    let api_session_prompt = has_openapi_route(&doc, "/api/session/{sessionID}/prompt", "post")
        || has_openapi_route(&doc, "/session/{sessionID}/prompt_async", "post");
    let api_session_wait = has_openapi_route(&doc, "/api/session/{sessionID}/wait", "post");
    let api_model_list = has_openapi_route(&doc, "/api/model", "get");
    let api_provider_list = has_openapi_route(&doc, "/api/provider", "get")
        || has_openapi_route(&doc, "/provider", "get");
    Ok(json!({
        "title": title,
        "version": version,
        "routes": {
            "session_create": api_session_create,
            "session_prompt": api_session_prompt,
            "session_wait": api_session_wait,
            "model_list": api_model_list,
            "provider_list": api_provider_list,
        }
    }))
}

fn http_probe(stream: TcpStream, port: u16) -> Result<Value, String> {
    let response = read_http_response(stream, port, "/")?;
    let body_prefix_len = response.body.len().min(8192);
    let body_prefix = String::from_utf8_lossy(&response.body[..body_prefix_len]);
    let opencode_hint = body_prefix.to_ascii_lowercase().contains("opencode");
    let password_configured = opencode_basic_auth_header().is_some();
    let doc_probe = probe_opencode_doc(port);
    let mut api_version_layer = if opencode_hint { "hinted" } else { "unknown" };
    let mut session_layer = "not_run";
    let mut api_capabilities = Value::Null;
    let mut doc_error = Value::Null;
    let mut attach_ready = false;
    let mut evidence_strength = "medium";
    let mut readiness = "http_responsive";
    match doc_probe {
        Ok(doc) => {
            api_version_layer = "passed";
            let session_create = doc
                .pointer("/routes/session_create")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            session_layer = if session_create {
                "route_available"
            } else {
                "failed"
            };
            attach_ready = password_configured && session_create;
            evidence_strength = if attach_ready { "strong" } else { "medium" };
            readiness = if attach_ready {
                "attach_ready"
            } else {
                "server_auth_required"
            };
            api_capabilities = doc;
        }
        Err(err) => {
            doc_error = json!(err);
        }
    }
    let confirmed_opencode_api = !api_capabilities.is_null();
    let unsafe_probe_layer = if confirmed_opencode_api || opencode_hint {
        "unsafe_to_probe"
    } else {
        "not_run"
    };
    let warning = if attach_ready {
        "OpenCode API schema and session-create route were observed; requested model availability and credentials were not verified"
    } else if confirmed_opencode_api && !password_configured {
        "OpenCode serve exposes secret-bearing API routes; set OPENCODE_SERVER_PASSWORD before Tachi uses opencode_serve transport"
    } else if password_configured {
        "HTTP response observed, but OpenCode attach readiness could not be fully verified"
    } else {
        "HTTP response observed; OpenCode session creation, requested model availability, and credentials were not verified"
    };
    Ok(json!({
        "reachable": true,
        "attach_ready": attach_ready,
        "probe": if api_capabilities.is_null() { "http" } else { "opencode_http_api" },
        "evidence_strength": evidence_strength,
        "readiness": readiness,
        "port": port,
        "http_status": response.status_code,
        "opencode_hint": opencode_hint,
        "server_password_configured": password_configured,
        "api_capabilities": api_capabilities,
        "layers": {
            "tcp_reachable": "passed",
            "http_health": "passed",
            "server_auth": if password_configured { "passed" } else { "failed" },
            "opencode_api_version": api_version_layer,
            "session_create_smoke": session_layer,
            "model_available": unsafe_probe_layer,
            "credential_ready": unsafe_probe_layer,
        },
        "unverified": {
            "session_create_smoke": "schema route observed only; probe does not create sessions to avoid mutating OpenCode state",
            "model_available": "not queried because OpenCode model endpoints include secret-bearing provider request bodies",
            "credential_ready": "not queried because OpenCode provider/config endpoints can expose raw credential material",
        },
        "sensitive_endpoints_skipped": ["/config", "/api/model", "/api/provider", "/config/providers"],
        "doc_error": doc_error,
        "warning": warning,
    }))
}

pub(crate) fn probe_harness_server_status(url: Option<&str>) -> Value {
    let Some(url) = url.map(str::trim).filter(|url| !url.is_empty()) else {
        return Value::Null;
    };
    let Some(port) = parse_local_http_port(url) else {
        return unsupported_probe("probe only supports local http server URLs");
    };

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let stream = match TcpStream::connect_timeout(&addr, HARNESS_PROBE_TIMEOUT) {
        Ok(stream) => stream,
        Err(err) => return tcp_only_status(port, false, Some(err.to_string())),
    };
    match http_probe(stream, port) {
        Ok(status) => status,
        Err(err) => tcp_only_status(port, true, Some(err)),
    }
}

pub(crate) fn harness_server_attach_ready(url: &str) -> bool {
    probe_harness_server_status(Some(url))
        .get("attach_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

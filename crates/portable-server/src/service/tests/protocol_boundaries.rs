use super::*;
use rmcp::ServiceExt;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct WirePeer {
    reader: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    next_id: i64,
}

impl WirePeer {
    async fn send(&mut self, message: Value) {
        let mut bytes = serde_json::to_vec(&message).expect("encode request");
        bytes.push(b'\n');
        self.writer.write_all(&bytes).await.expect("write request");
        self.writer.flush().await.expect("flush request");
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .await;
        let line = tokio::time::timeout(std::time::Duration::from_secs(5), self.reader.next_line())
            .await
            .expect("response timeout")
            .expect("read response")
            .expect("response line");
        let response: Value = serde_json::from_str(&line).expect("JSON-RPC response");
        assert_eq!(response["id"], id, "unexpected response: {response}");
        response
    }
}

fn legacy_routes(save_id: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("tools/list", json!({})),
        (
            "tools/call",
            json!({"name":"save", "arguments":{"id":save_id, "text":"protocol boundary fixture", "scope":"global"}}),
        ),
        (
            "completion/complete",
            json!({"ref":{"type":"ref/prompt", "name":"legacy-probe"}, "argument":{"name":"query", "value":""}}),
        ),
        ("prompts/list", json!({})),
        ("resources/list", json!({})),
        ("resources/templates/list", json!({})),
    ]
}

async fn assert_legacy_routes(
    peer: &mut WirePeer,
    server: &PortableServer,
    label: &str,
    meta: Value,
) {
    let save_id = format!("legacy-{label}");
    for (method, mut params) in legacy_routes(&save_id) {
        params["_meta"] = meta.clone();
        let response = peer.request(method, params).await;
        assert!(
            response.get("error").is_none(),
            "{label} {method}: {response}"
        );
        let result = &response["result"];
        match method {
            "tools/list" => assert!(result["tools"]
                .as_array()
                .expect("tools")
                .iter()
                .any(|tool| tool["name"] == "save")),
            "tools/call" => {
                assert_ne!(result["isError"], true, "{response}");
                let receipt: Value = serde_json::from_str(
                    result["content"][0]["text"].as_str().expect("save receipt"),
                )
                .expect("save JSON");
                assert_eq!(receipt["id"], save_id);
                assert_eq!(receipt["saved"], true);
            }
            "completion/complete" => assert_eq!(result["completion"]["values"], json!([])),
            "prompts/list" => assert_eq!(result["prompts"], json!([])),
            "resources/list" => assert_eq!(result["resources"], json!([])),
            "resources/templates/list" => assert_eq!(result["resourceTemplates"], json!([])),
            _ => unreachable!(),
        }
    }
    assert!(server
        .stores
        .global
        .store
        .lock()
        .expect("store")
        .get(&save_id)
        .expect("saved row")
        .is_some());
}

#[tokio::test]
async fn initialized_portable_session_rejects_partial_inline_context_without_mutation() {
    let server = boot(None, "portable-inline-boundary");
    let served = server.clone();
    let (client_io, server_io) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        served
            .serve(server_io)
            .await
            .expect("serve portable")
            .waiting()
            .await
            .expect("server stopped")
    });
    let (reader, writer) = tokio::io::split(client_io);
    let mut peer = WirePeer {
        reader: BufReader::new(reader).lines(),
        writer,
        next_id: 0,
    };
    let initialized = peer
        .request(
            "initialize",
            json!({
                "protocolVersion":"2025-11-25", "capabilities":{},
                "clientInfo":{"name":"legacy-boundary-client", "version":"1"}
            }),
        )
        .await;
    assert_eq!(
        initialized["result"]["protocolVersion"], "2025-11-25",
        "{initialized}"
    );
    peer.send(json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))
        .await;
    assert_legacy_routes(&mut peer, &server, "before", json!({})).await;

    // These are the SDK's three inline client-context keys, including valid,
    // partial and malformed values. Ordinary legacy metadata is tested below.
    let contexts = [
        (
            "complete modern",
            json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28", "io.modelcontextprotocol/clientCapabilities":{}, "io.modelcontextprotocol/clientInfo":{"name":"inline", "version":"1"}}),
        ),
        (
            "missing capabilities",
            json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28"}),
        ),
        (
            "malformed capabilities",
            json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28", "io.modelcontextprotocol/clientCapabilities":false}),
        ),
        (
            "null capabilities",
            json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28", "io.modelcontextprotocol/clientCapabilities":null}),
        ),
        (
            "capabilities only",
            json!({"io.modelcontextprotocol/clientCapabilities":{}}),
        ),
        (
            "capabilities malformed only",
            json!({"io.modelcontextprotocol/clientCapabilities":[]}),
        ),
        (
            "capabilities null only",
            json!({"io.modelcontextprotocol/clientCapabilities":null}),
        ),
        (
            "client info only",
            json!({"io.modelcontextprotocol/clientInfo":{"name":"inline", "version":"1"}}),
        ),
        (
            "client info malformed only",
            json!({"io.modelcontextprotocol/clientInfo":42}),
        ),
        (
            "client info null only",
            json!({"io.modelcontextprotocol/clientInfo":null}),
        ),
        (
            "malformed version",
            json!({"io.modelcontextprotocol/protocolVersion":42}),
        ),
        (
            "null version",
            json!({"io.modelcontextprotocol/protocolVersion":null}),
        ),
        (
            "old inline version",
            json!({"io.modelcontextprotocol/protocolVersion":"2025-11-25"}),
        ),
        (
            "complete old inline",
            json!({"io.modelcontextprotocol/protocolVersion":"2025-11-25", "io.modelcontextprotocol/clientCapabilities":{}}),
        ),
    ];
    let mut violations = Vec::new();
    for (index, (label, meta)) in contexts.into_iter().enumerate() {
        let save_id = format!("forbidden-inline-{index}");
        // The SDK rejects supported-shape modern versions before dispatch;
        // the portable boundary rejects the remaining inline contexts.
        let expected_code = if meta["io.modelcontextprotocol/protocolVersion"] == "2026-07-28" {
            -32022
        } else {
            -32600
        };
        for (method, mut params) in legacy_routes(&save_id) {
            params["_meta"] = meta.clone();
            let response = peer.request(method, params).await;
            if response["error"]["code"] != expected_code || response.get("result").is_some() {
                violations.push(format!(
                    "{label} {method}: expected {expected_code} without result, got {response}"
                ));
            }
        }
        if server
            .stores
            .global
            .store
            .lock()
            .expect("store")
            .get(&save_id)
            .expect("lookup forbidden row")
            .is_some()
        {
            violations.push(format!("{label}: forbidden save mutated the store"));
        }
    }
    // Both positive phases must run even against the buggy implementation.
    // Progress/extension metadata must not be mistaken for inline authority.
    assert_legacy_routes(
        &mut peer,
        &server,
        "after",
        json!({"progressToken":"legacy-progress", "fixtureAnnotation":"ordinary-extension"}),
    )
    .await;
    let rows = server
        .stores
        .global
        .store
        .lock()
        .expect("store")
        .stats(false)
        .expect("entry census")
        .total;
    if rows != 2 {
        violations.push(format!(
            "forbidden inline calls changed entry census: expected 2 legacy rows, got {rows}"
        ));
    }
    drop(peer);
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .expect("server shutdown timeout")
        .expect("server task");
    assert!(
        violations.is_empty(),
        "initialized portable session accepted inline authority:\n{}",
        violations.join("\n")
    );
}

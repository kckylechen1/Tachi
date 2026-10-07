use super::*;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE};
use tokio_util::sync::CancellationToken;

async fn rpc(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderMap,
    request: Value,
) -> (Value, HeaderMap) {
    let response = client
        .post(url)
        .headers(headers.clone())
        .json(&request)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let response_headers = response.headers().clone();
    let body = response.text().await.unwrap();
    let result = serde_json::from_str(&body).unwrap_or_else(|_| {
        body.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .find_map(|data| serde_json::from_str(data.trim()).ok())
            .expect("JSON or SSE response")
    });
    (result, response_headers)
}

async fn initialize(client: &reqwest::Client, url: &str, identity: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert("x-tachi-profile", HeaderValue::from_static("admin"));
    headers.insert(
        "x-tachi-internal-proxy-token",
        HeaderValue::from_static("public-admission-fixture-proxy"),
    );
    headers.insert("x-tachi-agent-identity", identity.parse().unwrap());
    let (initialized, response_headers) = rpc(
        client,
        url,
        &headers,
        json!({
            "jsonrpc":"2.0", "id":1, "method":"initialize",
            "params":{"protocolVersion":"2025-11-25", "capabilities":{},
                "clientInfo":{"name":"public-admission-fixture", "version":"1"}}
        }),
    )
    .await;
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    headers.remove("x-tachi-agent-identity");
    headers.insert("mcp-session-id", response_headers["mcp-session-id"].clone());
    headers.insert(
        "mcp-protocol-version",
        HeaderValue::from_static("2025-11-25"),
    );
    let response = client
        .post(url)
        .headers(headers.clone())
        .json(&json!({"jsonrpc":"2.0", "method":"notifications/initialized"}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    headers
}

async fn tool(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderMap,
    name: &str,
    arguments: Value,
) -> Value {
    let (response, _) = rpc(client, url, headers, json!({
        "jsonrpc":"2.0", "id":2, "method":"tools/call", "params":{"name":name,"arguments":arguments}
    })).await;
    assert!(response.get("error").is_none(), "{response}");
    response["result"].clone()
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().expect("tool text")
}

async fn board(client: &reqwest::Client, url: &str, headers: &HeaderMap) -> Value {
    let result = tool(
        client,
        url,
        headers,
        "tachi_task",
        json!({"action":"board","format":"json"}),
    )
    .await;
    assert_ne!(result["isError"], true, "{result}");
    serde_json::from_str(text(&result)).unwrap()
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Reads only public MCP responses for the Host reference. Fixture setup owns
/// the pre-existing worker grant/claim; the client never obtains a DB key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialized_public_board_reference_attaches_and_reconnect_rejects_stale_foreign_forged_refs(
) {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join("global")).unwrap();
    let server = MemoryServer::new_with_home_for_test(
        home.path().join("global/memory.db"),
        None,
        home.path().to_path_buf(),
    )
    .unwrap();
    seed_valid_admission(&server);
    server.set_daemon_proxy_token("public-admission-fixture-proxy".into());
    let stop = CancellationToken::new();
    let _cleanup = CancelOnDrop(stop.clone());
    let router = crate::bootstrap::daemon_http_router(server.clone(), stop.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let shutdown = stop.clone();
    let serving = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap();
    let first = initialize(&client, &url, "host-1").await;
    let before = board(&client, &url, &first).await;
    let reference = before["admission_receipt_ref"]
        .as_str()
        .expect("initialized public receipt reference")
        .to_string();
    assert!(reference.starts_with("conn-"));
    assert_eq!(
        before["admission_receipt_ref"],
        board(&client, &url, &first).await["admission_receipt_ref"],
        "read does not mint a new reference"
    );
    let markdown = tool(
        &client,
        &url,
        &first,
        "tachi_task",
        json!({"action":"board","format":"markdown"}),
    )
    .await;
    assert!(text(&markdown).contains(&format!("admission_receipt_ref: `{reference}`")));

    let mut params = json!({
        "action":"attach_session", "host_identity":"host-1", "agent_identity_id":"agent-1",
        "work_claim_id":"claim-1", "expected_transition_revision":0, "protocol_version":1,
        "adapter_connection_identity":"adapter-public", "remote_session_id":"remote-public",
        "contract_digest":"contract-digest", "session_capabilities":["observe","load"],
        "tool_profile":"delegate", "capability_class":"tachi", "idempotency_key":"public-first",
        "admission_receipt_ref":reference,
    });
    let attached = tool(&client, &url, &first, "tachi_agent_eval", params.clone()).await;
    assert_ne!(attached["isError"], true, "{attached}");
    let attached: Value = serde_json::from_str(text(&attached)).unwrap();
    assert_eq!(attached["admission"], "created");
    assert_eq!(row_count(&server), 1);
    let reconnected = initialize(&client, &url, "host-1").await;
    let current = board(&client, &url, &reconnected).await["admission_receipt_ref"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(reference, current);
    let mut current_params = params.clone();
    current_params["admission_receipt_ref"] = json!(current);
    current_params["idempotency_key"] = json!("public-current");
    current_params["remote_session_id"] = json!("remote-current");
    let attached_current = tool(
        &client,
        &url,
        &reconnected,
        "tachi_agent_eval",
        current_params,
    )
    .await;
    assert_ne!(attached_current["isError"], true, "{attached_current}");
    let attached_current: Value = serde_json::from_str(text(&attached_current)).unwrap();
    assert_eq!(attached_current["admission"], "created");
    assert_eq!(row_count(&server), 2);
    // Prove the new connection can record a fact with its own reference.
    let fact_params = json!({
        "action":"mark_session_connection", "host_identity":"host-1",
        "admission_receipt_ref":current, "attachment_id":attached_current["attachment_id"],
        "connection_fact":"disconnected",
    });
    let fact = tool(
        &client,
        &url,
        &reconnected,
        "tachi_agent_eval",
        fact_params.clone(),
    )
    .await;
    assert_ne!(fact["isError"], true, "{fact}");
    let fact: Value = serde_json::from_str(text(&fact)).unwrap();
    assert_eq!(
        fact["changed"], true,
        "a valid reference must reach the fact writer"
    );
    let stored_after_fact = stored_rows(&server);
    let foreign = initialize(&client, &url, "host-foreign").await;
    let foreign_ref = board(&client, &url, &foreign).await["admission_receipt_ref"]
        .as_str()
        .unwrap()
        .to_string();
    for (name, rejected_ref, reason) in [
        ("old", reference, "holder_mismatch"),
        ("foreign", foreign_ref, "holder_mismatch"),
        (
            "forged",
            "conn-invented".into(),
            "admission receipt is missing",
        ),
    ] {
        let mut rejected = params.clone();
        rejected["idempotency_key"] = json!(format!("public-rejected-{name}"));
        rejected["remote_session_id"] = json!(format!("remote-{name}"));
        rejected["admission_receipt_ref"] = json!(rejected_ref);
        let result = tool(
            &client,
            &url,
            &reconnected,
            "tachi_agent_eval",
            rejected.clone(),
        )
        .await;
        assert_eq!(result["isError"], true, "{result}");
        assert!(text(&result).contains(reason), "{result}");
        assert_eq!(row_count(&server), 2, "rejected references create no facts");
        let mut rejected_fact = fact_params.clone();
        rejected_fact["admission_receipt_ref"] = rejected["admission_receipt_ref"].clone();
        rejected_fact["connection_fact"] = json!("reconnect_failed");
        let fact_reason = if name == "old" {
            // The old attachment matches this old reference, so the durable
            // current-connection check, not lookup filtering, must refuse it.
            rejected_fact["attachment_id"] = attached["attachment_id"].clone();
            "holder_mismatch"
        } else {
            // Foreign/invented references do not identify this attachment:
            // preserve the existing typed, non-disclosing NotFound boundary.
            "Not found: harness session spine was not found for the current host connection"
        };
        let result = tool(
            &client,
            &url,
            &reconnected,
            "tachi_agent_eval",
            rejected_fact,
        )
        .await;
        assert_eq!(result["isError"], true, "{result}");
        assert!(text(&result).contains(fact_reason), "{name}: {result}");
        assert_eq!(
            stored_rows(&server),
            stored_after_fact,
            "refused facts preserve the attachment"
        );
    }
    // A public reference never supplies the worker's independent grant.
    set_capability_json(&server, None);
    params["idempotency_key"] = json!("public-no-grant");
    params["remote_session_id"] = json!("remote-no-grant");
    params["admission_receipt_ref"] = json!(current);
    let denied = tool(&client, &url, &reconnected, "tachi_agent_eval", params).await;
    assert_eq!(denied["isError"], true);
    assert!(text(&denied).contains("capability"), "{denied}");
    assert_eq!(row_count(&server), 2);
    stop.cancel();
    serving.await.unwrap();
}

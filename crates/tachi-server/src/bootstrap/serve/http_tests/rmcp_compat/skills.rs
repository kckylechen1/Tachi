use super::*;
use sha2::{Digest, Sha256};

async fn skill_rpc(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    params: serde_json::Value,
    extra_headers: &[(&str, &str)],
) -> serde_json::Value {
    let mut headers = vec![
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", method),
    ];
    if method == "resources/read" {
        headers.push(("mcp-name", params["uri"].as_str().expect("URI")));
    }
    headers.extend_from_slice(extra_headers);
    let response = client
        .post(url)
        .headers(http_headers(&headers))
        .json(&json!({"jsonrpc":"2.0", "id":901, "method":method, "params":params}))
        .send()
        .await
        .expect("skill request");
    assert!(!response.headers().contains_key("mcp-session-id"));
    parse_http_mcp_payload(&response.text().await.expect("skill response"), 901)
}

#[test]
fn bundled_skill_crosses_modern_and_legacy_http_without_project_binding() {
    let temp = tempfile::tempdir().expect("tempdir");
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).expect("server");
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let client = reqwest::Client::new();
            let uri = crate::server_instructions::SKILL_URI;
            let meta = modern_meta(json!({}));
            let discovery = skill_rpc(
                &client,
                &daemon.url,
                "server/discover",
                json!({"_meta":meta}),
                &[],
            )
            .await;
            assert_eq!(
                discovery["result"]["capabilities"]["extensions"]
                    [crate::server_instructions::SKILL_EXTENSION],
                json!({})
            );
            assert!(discovery["result"]["capabilities"]["resources"].is_object());
            assert!(discovery["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains(uri));

            let listing = skill_rpc(
                &client,
                &daemon.url,
                "skills/list",
                json!({"_meta":meta}),
                &[],
            )
            .await;
            assert!(listing.get("error").is_none(), "{listing:#}");
            assert_eq!(listing["result"]["skills"].as_array().unwrap().len(), 1);
            assert!(listing["result"].get("nextCursor").is_none());
            let entry = &listing["result"]["skills"][0];
            let get = skill_rpc(
                &client,
                &daemon.url,
                "skills/get",
                json!({"_meta":meta, "uri":uri}),
                &[],
            )
            .await;
            assert_eq!(&get["result"]["skill"], entry);
            assert!(get["result"].get("nextCursor").is_none());
            let read = skill_rpc(
                &client,
                &daemon.url,
                "resources/read",
                json!({"_meta":meta, "uri":uri}),
                &[],
            )
            .await;
            let content = &read["result"]["contents"][0];
            let body = content["text"].as_str().expect("skill text");
            assert_eq!(content["mimeType"], "text/markdown");
            assert_eq!(content["uri"], uri);
            assert_eq!(body, crate::server_instructions::SKILL_TEXT);
            let frontmatter: serde_json::Value = serde_json::from_str(
                body.strip_prefix("---\n")
                    .unwrap()
                    .split_once("\n---\n")
                    .unwrap()
                    .0,
            )
            .unwrap();
            assert_eq!(entry["frontmatter"], frontmatter);
            assert_eq!(frontmatter["name"], "tachi");
            assert_eq!(entry["uri"], uri);
            assert_eq!(
                entry["resources"],
                json!([{
                    "uri":uri, "size":body.len(),
                    "digest":format!("sha256:{:x}", Sha256::digest(body.as_bytes()))
                }])
            );
            for response in [&listing, &get, &read] {
                assert_eq!(response["result"]["resultType"], "complete");
                assert_eq!(response["result"]["ttlMs"], 300_000);
                assert_eq!(response["result"]["cacheScope"], "public");
            }

            // `skills/list` params are rmcp's typed paginated params. Tachi
            // issues no cursors, so any string cursor is refused below; a
            // cursor rmcp cannot read as a string (null, or a wrong type) is
            // decoded by the SDK as absent params, which serve the one page.
            for cursor in [json!(null), json!(0)] {
                let unread = skill_rpc(
                    &client,
                    &daemon.url,
                    "skills/list",
                    json!({"_meta":meta,"cursor":cursor}),
                    &[],
                )
                .await;
                assert_eq!(unread["result"], listing["result"], "{cursor}: {unread:#}");
            }

            for (method, params, headers, code) in [
                (
                    "skills/list",
                    json!({"_meta":meta,"cursor":"bogus"}),
                    vec![],
                    -32602,
                ),
                ("skills/get", json!({"_meta":meta}), vec![], -32602),
                (
                    "skills/get",
                    json!({"_meta":meta,"uri":null}),
                    vec![],
                    -32602,
                ),
                ("skills/get", json!({"_meta":meta,"uri":42}), vec![], -32602),
                (
                    "skills/get",
                    json!({"_meta":meta,"uri":"skill://tachi/../secret"}),
                    vec![],
                    -32602,
                ),
                (
                    "resources/read",
                    json!({"_meta":meta,"uri":"skill://tachi/../secret"}),
                    vec![],
                    -32602,
                ),
                ("skills/list", json!({}), vec![], -32602),
                (
                    "skills/list",
                    json!({"_meta":{
                        "io.modelcontextprotocol/protocolVersion":"2026-07-28"
                    }}),
                    vec![],
                    -32602,
                ),
                (
                    "skills/list",
                    json!({"_meta":modern_meta(json!({"tachiProject":"body"}))}),
                    vec![("x-tachi-project", "header")],
                    -32020,
                ),
                (
                    "skills/get",
                    json!({"_meta":modern_meta(json!({"tachiProfile":"worker"})), "uri":uri}),
                    vec![("x-tachi-profile", "standard")],
                    -32020,
                ),
                (
                    "resources/read",
                    json!({"_meta":modern_meta(json!({"tachiProject":"body"})), "uri":uri}),
                    vec![("x-tachi-project", "header")],
                    -32020,
                ),
                (
                    "resources/directory/read",
                    json!({"_meta":meta,"uri":"skill://tachi"}),
                    vec![],
                    -32601,
                ),
            ] {
                let result = skill_rpc(&client, &daemon.url, method, params, &headers).await;
                assert_eq!(result["error"]["code"], code, "{method}: {result:#}");
                assert!(result.get("result").is_none(), "{method}: {result:#}");
            }

            let (legacy_client, headers, init) =
                http_mcp_initialize(&daemon.url, http_headers(&[]), None).await;
            assert!(init["result"]["capabilities"]["extensions"]
                .get(crate::server_instructions::SKILL_EXTENSION)
                .is_none());
            http_mcp_initialized(&legacy_client, &daemon.url, headers.clone()).await;
            for (method, params) in [
                ("resources/read", json!({"uri":uri})),
                ("skills/list", json!({})),
                ("skills/get", json!({"uri":uri})),
            ] {
                let response = legacy_client
                    .post(&daemon.url)
                    .headers(headers.clone())
                    .json(&json!({"jsonrpc":"2.0", "id":902,"method":method,"params":params}))
                    .send()
                    .await
                    .expect("legacy request");
                let result = parse_http_mcp_payload(&response.text().await.unwrap(), 902);
                if method == "resources/read" {
                    assert_eq!(result["result"]["contents"][0]["text"], body);
                    assert!(result["result"].get("ttlMs").is_none());
                    assert!(result["result"].get("cacheScope").is_none());
                } else {
                    assert_eq!(result["error"]["code"], -32600, "{result:#}");
                }
            }
            cancel.cancel();
            task.await.expect("daemon stopped");
        });
    });
}

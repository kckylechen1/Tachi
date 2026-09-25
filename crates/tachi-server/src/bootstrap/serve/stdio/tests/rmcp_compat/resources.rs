use super::*;

async fn modern_resource_rpc(
    client: &reqwest::Client,
    url: &str,
    id: i64,
    method: &str,
    params: serde_json::Value,
    extra_headers: &[(&str, &str)],
) -> serde_json::Value {
    let mut pairs = vec![
        ("mcp-protocol-version", "2026-07-28"),
        ("mcp-method", method),
    ];
    if method == "resources/read" {
        pairs.push(("mcp-name", params["uri"].as_str().expect("resource URI")));
    }
    pairs.extend_from_slice(extra_headers);
    let response = client
        .post(url)
        .headers(http_headers(&pairs))
        .json(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
        .send()
        .await
        .unwrap_or_else(|error| panic!("modern {method}: {error}"));
    parse_http_mcp_payload(
        &response
            .text()
            .await
            .unwrap_or_else(|error| panic!("modern {method} response body: {error}")),
        id,
    )
}

async fn legacy_resource_rpc(
    client: &reqwest::Client,
    url: &str,
    headers: reqwest::header::HeaderMap,
    id: i64,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let response = client
        .post(url)
        .headers(headers)
        .json(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
        .send()
        .await
        .unwrap_or_else(|error| panic!("legacy {method}: {error}"));
    parse_http_mcp_payload(
        &response
            .text()
            .await
            .unwrap_or_else(|error| panic!("legacy {method} response body: {error}")),
        id,
    )
}

fn resource_text(response: &serde_json::Value) -> &str {
    response["result"]["contents"]
        .as_array()
        .and_then(|contents| contents.first())
        .and_then(|content| content["text"].as_str())
        .unwrap_or_else(|| panic!("resource response has no text content: {response:#}"))
}

#[test]
fn resources_cross_modern_and_legacy_http_and_stdio_proxy_boundaries() {
    let project_name = "resource-wire-project";
    let temp = tempfile::tempdir().expect("tempdir");
    let (server, project_db) = crate::tests::make_server_with_project_fixture(project_name);
    let sentinel = "ResourceWireExactBodySentinel";
    let mut entry = crate::tests::make_entry("resource-wire-memory-id");
    entry.summary = format!("{sentinel} summary");
    entry.text = format!("{sentinel}: exact text from the current bound-project row");
    entry.keywords = vec![sentinel.to_string()];
    server
        .with_named_project_store(project_name, |store| {
            store
                .upsert(&entry)
                .map_err(|error| format!("seed resource wire row: {error}"))
        })
        .expect("seed bound project resource row");

    // Plant a real encrypted private partition under a second manifest-bound
    // project name. Its row deliberately looks like an ordinary project-scope
    // memory; generic project lookup must reject the sealed file itself.
    let private_project_name = "resource-wire-sealed-private";
    let private_estate = temp.path().join("private-estate");
    let private_body = "SealedPrivateResourceBodySentinel";
    let mut private_entry = crate::tests::make_entry("resource-private-sealed-id");
    private_entry.scope = "project".to_string();
    private_entry.text = private_body.to_string();
    let private_context = memcore::PrivatePartitionOpenContext {
        trust_domain_id: memcore::TrustDomainId::new("td-resource-wire").unwrap(),
        subject_id: memcore::SubjectId::new("subject-resource-wire").unwrap(),
        receipt: memcore::CapabilityReceipt::new("receipt-resource-wire").unwrap(),
        capabilities: [
            memcore::PartitionCapability::Read,
            memcore::PartitionCapability::Write,
        ]
        .into_iter()
        .collect(),
        key_version: "kv1".to_string(),
        revoked: false,
    };
    let private_keys = memcore::StaticKeyProvider::new([17; 32])
        .with_admission(
            "td-resource-wire",
            "subject-resource-wire",
            "receipt-resource-wire",
            [
                memcore::PartitionCapability::Read,
                memcore::PartitionCapability::Write,
            ],
            "kv1",
        )
        .unwrap();
    let sealed_path = {
        let mut partition =
            memcore::PrivatePartition::open(&private_estate, &private_context, &private_keys)
                .expect("open private partition with admitted context");
        partition
            .insert_if_absent(&private_entry)
            .expect("insert private partition row");
        let path = private_estate
            .join(&partition.identity().partition_id)
            .join("partition.sealed");
        partition
            .close()
            .expect("persist encrypted private partition");
        path
    };
    {
        let reopened =
            memcore::PrivatePartition::open(&private_estate, &private_context, &private_keys)
                .expect("reopen encrypted private partition through its open context");
        assert_eq!(
            reopened
                .get(&private_entry.id)
                .expect("read encrypted private fixture")
                .expect("private row persisted")
                .text,
            private_body
        );
    }
    assert!(
        memcore::MemoryStore::open_with_label(
            sealed_path.to_str().expect("UTF-8 sealed path"),
            private_project_name,
        )
        .is_err(),
        "generic project-store opening must refuse an encrypted partition"
    );
    let manifest_path = server.tachi_home_dir().join("manifest.json");
    let mut manifest = crate::manifest::Manifest::load_or_empty(&manifest_path);
    manifest.dbs.push(crate::manifest::DbEntry {
        path: sealed_path.display().to_string(),
        role: crate::manifest::DbRole::Project,
        owner: "test".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: chrono::Utc::now().to_rfc3339(),
        last_classification: "healthy".to_string(),
        scope_hint: format!("project:{private_project_name}"),
        notes: String::new(),
    });
    manifest
        .save(&manifest_path)
        .expect("bind sealed fixture path");
    assert_eq!(
        server
            .resolve_server_named_project_db_path(private_project_name)
            .expect("resolve sealed private project binding"),
        std::fs::canonicalize(&sealed_path).expect("canonical sealed private fixture path")
    );

    let global_db = server.global_db_path_buf();
    let daemon_server = std::ops::Deref::deref(&server).clone();
    let mutation_server = std::ops::Deref::deref(&server).clone();
    let app_home = temp.path().to_path_buf();

    with_tachi_home(temp.path(), || {
        test_runtime().block_on(async move {
            let (daemon, cancel, task) =
                spawn_test_http_daemon(daemon_server, &global_db).await;
            let client = reqwest::Client::new();
            let identity = json!({
                "tachiProject": project_name,
                "tachiProfile": "standard"
            });

            let search = modern_http_tool_call(
                &client,
                &daemon.url,
                801,
                "tachi_memory",
                identity.clone(),
                json!({
                    "action":"search", "query":sentinel, "scope":"memory",
                    "project":project_name, "format":"json"
                }),
            )
            .await;
            assert!(search.get("error").is_none(), "search failed: {search:#}");
            let blocks = search["result"]["content"]
                .as_array()
                .expect("search content blocks");
            assert_eq!(blocks.len(), 2, "search returns text plus one ResourceLink");
            assert_eq!(blocks[0]["type"], "text", "original text stays first");
            assert_eq!(
                blocks[1]["type"], "resource_link",
                "the eligible hit is an additive ResourceLink"
            );
            let search_text = blocks[0]["text"].as_str().expect("original search text");
            assert!(search_text.contains(sentinel), "search text changed: {search_text}");
            let uri = blocks[1]["uri"]
                .as_str()
                .expect("ResourceLink URI")
                .to_string();

            // Real daemon modern HTTP read, cache envelope, and empty resource
            // catalogs all exercise the production ServerHandler methods.
            let modern_read = modern_resource_rpc(
                &client,
                &daemon.url,
                802,
                "resources/read",
                json!({"_meta":modern_meta(identity.clone()), "uri":uri.clone()}),
                &[],
            )
            .await;
            assert!(modern_read.get("error").is_none(), "read failed: {modern_read:#}");
            assert_eq!(resource_text(&modern_read), entry.text);
            assert_eq!(
                modern_read["result"]["contents"][0]["mimeType"],
                "text/plain"
            );
            assert_eq!(modern_read["result"]["ttlMs"], 0);
            assert_eq!(modern_read["result"]["cacheScope"], "private");

            for (id, method, collection) in [
                (803, "resources/list", "resources"),
                (804, "resources/templates/list", "resourceTemplates"),
            ] {
                let listing = modern_resource_rpc(
                    &client,
                    &daemon.url,
                    id,
                    method,
                    json!({"_meta":modern_meta(identity.clone())}),
                    &[],
                )
                .await;
                assert!(listing.get("error").is_none(), "{method}: {listing:#}");
                assert_eq!(listing["result"][collection].as_array().map(Vec::len), Some(0));
                assert_eq!(listing["result"]["ttlMs"], 0);
                assert_eq!(listing["result"]["cacheScope"], "private");
            }

            let malformed = modern_resource_rpc(
                &client,
                &daemon.url,
                805,
                "resources/read",
                json!({"_meta":modern_meta(identity.clone()), "uri":"tachi-memory://v1/private-path-sentinel"}),
                &[],
            )
            .await;
            assert_eq!(malformed["error"]["message"], "resource unavailable");
            assert_eq!(malformed["error"]["code"], -32602);
            assert!(!malformed.to_string().contains("private-path-sentinel"));

            let private_uri = format!(
                "tachi-memory://v1/{}/{}/{}/{}",
                "1".repeat(64),
                private_entry.id,
                private_entry.revision,
                "2".repeat(64),
            );
            let sealed_read = modern_resource_rpc(
                &client,
                &daemon.url,
                818,
                "resources/read",
                json!({
                    "_meta":modern_meta(json!({
                        "tachiProject":private_project_name,
                        "tachiProfile":"standard"
                    })),
                    "uri":private_uri
                }),
                &[],
            )
            .await;
            assert_eq!(
                sealed_read["error"]["code"], malformed["error"]["code"],
                "sealed private database read must use the generic resource refusal: {sealed_read:#}"
            );
            assert_eq!(sealed_read["error"]["message"], "resource unavailable");
            assert!(!sealed_read.to_string().contains(private_body));
            assert!(!sealed_read
                .to_string()
                .contains(&sealed_path.display().to_string()));

            let public_peer_read = modern_resource_rpc(
                &client,
                &daemon.url,
                819,
                "resources/read",
                json!({"_meta":modern_meta(identity.clone()), "uri":uri.clone()}),
                &[],
            )
            .await;
            assert!(
                public_peer_read.get("error").is_none(),
                "a public bound-project peer remains readable: {public_peer_read:#}"
            );
            assert_eq!(resource_text(&public_peer_read), entry.text);

            let forged_project = modern_resource_rpc(
                &client,
                &daemon.url,
                806,
                "resources/read",
                json!({"_meta":modern_meta(identity.clone()), "uri":uri.clone()}),
                &[("x-tachi-project", "different-project")],
            )
            .await;
            // Identity validation rejects this at the HTTP transport boundary,
            // before ServerHandler::read_resource. Preserve that actual scope:
            // the protocol reports the dedicated header-mismatch code/message.
            assert_eq!(forged_project["error"]["code"], -32020);
            assert!(forged_project["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("x-tachi-project header identity")
                    && message.contains("does not match request _meta identity")));
            assert!(!forged_project.to_string().contains(&entry.text));
            assert!(!forged_project.to_string().contains(&project_db.display().to_string()));

            let forged_profile = modern_resource_rpc(
                &client,
                &daemon.url,
                807,
                "resources/read",
                json!({
                    "_meta":modern_meta(json!({
                        "tachiProject":project_name,
                        "tachiProfile":"admin"
                    })),
                    "uri":uri.clone()
                }),
                &[],
            )
            .await;
            assert_eq!(forged_profile["error"]["code"], -32602);
            assert_eq!(
                forged_profile["error"]["message"],
                "HTTP direct-connect profile 'admin' requires explicit authorization that caller-supplied profile metadata cannot provide; select Ops/admin only from a trusted local process configuration"
            );
            assert!(!forged_profile.to_string().contains(&entry.text));
            assert!(!forged_profile.to_string().contains(&project_db.display().to_string()));

            // Legacy HTTP keeps its initialize-bound project and adapts the
            // same production read without modern-only cache fields.
            let (legacy_client, legacy_headers, init) = http_mcp_initialize(
                &daemon.url,
                http_headers(&[]),
                Some(json!({
                    "tachiProject":project_name,
                    "tachiProfile":"standard"
                })),
            )
            .await;
            assert!(init.get("error").is_none(), "legacy initialize: {init:#}");
            http_mcp_initialized(&legacy_client, &daemon.url, legacy_headers.clone()).await;
            let legacy_read = legacy_resource_rpc(
                &legacy_client,
                &daemon.url,
                legacy_headers.clone(),
                808,
                "resources/read",
                json!({"uri":uri.clone()}),
            )
            .await;
            assert!(legacy_read.get("error").is_none(), "legacy read: {legacy_read:#}");
            assert_eq!(resource_text(&legacy_read), entry.text);
            assert!(legacy_read["result"].get("ttlMs").is_none());
            assert!(legacy_read["result"].get("cacheScope").is_none());
            let legacy_malformed = legacy_resource_rpc(
                &legacy_client,
                &daemon.url,
                legacy_headers.clone(),
                823,
                "resources/read",
                json!({"uri":"tachi-memory://v1/private-path-sentinel"}),
            )
            .await;
            // SEP-2164 upgrades resource-not-found to invalid-params only for
            // modern peers. Compare each proxy with its matching wire version.
            assert_eq!(legacy_malformed["error"]["code"], -32002);
            assert_eq!(legacy_malformed["error"]["message"], "resource unavailable");
            assert!(!legacy_malformed.to_string().contains("private-path-sentinel"));
            for (id, method, collection) in [
                (809, "resources/list", "resources"),
                (810, "resources/templates/list", "resourceTemplates"),
            ] {
                let listing = legacy_resource_rpc(
                    &legacy_client,
                    &daemon.url,
                    legacy_headers.clone(),
                    id,
                    method,
                    json!({}),
                )
                .await;
                assert!(listing.get("error").is_none(), "legacy {method}: {listing:#}");
                assert_eq!(listing["result"][collection].as_array().map(Vec::len), Some(0));
            }

            let configured_profile = tachi_hub::default_tool_profile();
            let profile_name = configured_profile.as_str().to_string();
            let mut proxy = identity_probe_proxy();
            proxy.tool_profile = Some(configured_profile);
            proxy.client_project = Some(project_name.to_string());
            proxy.app_home = app_home;
            proxy.global_db_path = global_db.clone();
            proxy.project_db_path = Some(project_db.clone());
            *proxy.daemon.write().expect("proxy daemon lock") = daemon.clone();
            let proxy_meta = modern_meta(json!({
                "tachiProject":project_name,
                "tachiProfile":profile_name
            }));
            let modern_stdio = stdio_responses(
                proxy.clone(),
                &[
                    json!({"jsonrpc":"2.0", "id":811, "method":"resources/list", "params":{"_meta":proxy_meta.clone()}}),
                    json!({"jsonrpc":"2.0", "id":812, "method":"resources/templates/list", "params":{"_meta":proxy_meta.clone()}}),
                    json!({"jsonrpc":"2.0", "id":813, "method":"resources/read", "params":{"_meta":proxy_meta.clone(), "uri":uri.clone()}}),
                    json!({"jsonrpc":"2.0", "id":821, "method":"resources/read", "params":{"_meta":proxy_meta.clone(), "uri":"tachi-memory://v1/private-path-sentinel"}}),
                ],
            )
            .await;
            assert_eq!(modern_stdio[0]["result"]["resources"].as_array().map(Vec::len), Some(0));
            assert_eq!(modern_stdio[0]["result"]["ttlMs"], 0);
            assert_eq!(modern_stdio[0]["result"]["cacheScope"], "private");
            assert_eq!(modern_stdio[1]["result"]["resourceTemplates"].as_array().map(Vec::len), Some(0));
            assert_eq!(modern_stdio[1]["result"]["ttlMs"], 0);
            assert_eq!(modern_stdio[1]["result"]["cacheScope"], "private");
            assert_eq!(modern_stdio[2]["result"]["resultType"], "complete");
            assert_eq!(modern_stdio[2]["result"]["ttlMs"], 0);
            assert_eq!(modern_stdio[2]["result"]["cacheScope"], "private");
            assert_eq!(resource_text(&modern_stdio[2]), entry.text);
            assert_eq!(modern_stdio[3]["error"], malformed["error"]);

            let legacy_stdio = stdio_responses(
                proxy,
                &[
                    json!({
                        "jsonrpc":"2.0", "id":813, "method":"initialize",
                        "params":{
                            "protocolVersion":"2024-11-05", "capabilities":{},
                            "clientInfo":{"name":"resource-legacy-proxy", "version":"1"}
                        }
                    }),
                    json!({"jsonrpc":"2.0", "method":"notifications/initialized"}),
                    json!({"jsonrpc":"2.0", "id":814, "method":"resources/list", "params":{}}),
                    json!({"jsonrpc":"2.0", "id":815, "method":"resources/templates/list", "params":{}}),
                    json!({"jsonrpc":"2.0", "id":816, "method":"resources/read", "params":{"uri":uri.clone()}}),
                    json!({"jsonrpc":"2.0", "id":822, "method":"resources/read", "params":{"uri":"tachi-memory://v1/private-path-sentinel"}}),
                ],
            )
            .await;
            assert_eq!(legacy_stdio[0]["result"]["protocolVersion"], "2024-11-05");
            assert_eq!(legacy_stdio[1]["result"]["resources"].as_array().map(Vec::len), Some(0));
            assert_eq!(legacy_stdio[2]["result"]["resourceTemplates"].as_array().map(Vec::len), Some(0));
            assert_eq!(resource_text(&legacy_stdio[3]), entry.text);
            assert!(legacy_stdio[3]["result"].get("ttlMs").is_none());
            assert!(legacy_stdio[3]["result"].get("cacheScope").is_none());
            assert_eq!(legacy_stdio[4]["error"], legacy_malformed["error"]);

            let out_of_band_body = "ResourceWireOutOfBandChangedBodySentinel";
            {
                // Deliberately model a legacy/out-of-band writer that changes
                // persisted content without advancing revision. This one test
                // is explicitly authorized to use an unrestricted fixture
                // connection; production and MemoryStore write guards remain
                // untouched.
                let connection = rusqlite::Connection::open(&project_db)
                    .expect("open isolated fixture database for legacy-write simulation");
                connection
                    .execute(
                        "UPDATE memories SET text = ?1 WHERE id = ?2",
                        rusqlite::params![out_of_band_body, entry.id],
                    )
                    .expect("simulate persisted body mutation without revision advance");
            }
            let persisted_after_out_of_band_write = mutation_server
                .with_named_project_store_read_identity_checked(project_name, |store| {
                    store
                        .get_active_resource_entry(&entry.id)
                        .map_err(|error| format!("read out-of-band fixture mutation: {error}"))?
                        .ok_or_else(|| "out-of-band fixture row disappeared".to_string())
                })
                .expect("read persisted fixture mutation through the project store");
            assert_eq!(persisted_after_out_of_band_write.text, out_of_band_body);
            assert_eq!(
                persisted_after_out_of_band_write.revision, entry.revision,
                "fixture mutation must retain the issued URI's revision"
            );

            let digest_stale_read = modern_resource_rpc(
                &client,
                &daemon.url,
                820,
                "resources/read",
                json!({"_meta":modern_meta(identity.clone()), "uri":uri.clone()}),
                &[],
            )
            .await;
            assert_eq!(
                digest_stale_read["error"]["code"], malformed["error"]["code"]
            );
            assert_eq!(digest_stale_read["error"]["message"], "resource unavailable");
            assert!(!digest_stale_read.to_string().contains(&entry.text));
            assert!(!digest_stale_read.to_string().contains(out_of_band_body));
            assert!(!digest_stale_read
                .to_string()
                .contains(&project_db.display().to_string()));

            mutation_server
                .with_named_project_store(project_name, |store| {
                    let updated = store
                        .update_with_revision(
                            &entry.id,
                            "body after the issued reference became stale",
                            "updated resource wire summary",
                            "fixture",
                            &json!({}),
                            None,
                            entry.revision,
                        )
                        .map_err(|error| format!("update wire fixture: {error}"))?;
                    if updated {
                        Ok(())
                    } else {
                        Err("wire fixture revision update did not apply".to_string())
                    }
                })
                .expect("advance bound-project row revision");
            let stale_read = modern_resource_rpc(
                &client,
                &daemon.url,
                817,
                "resources/read",
                json!({"_meta":modern_meta(identity), "uri":uri}),
                &[],
            )
            .await;
            assert_eq!(stale_read["error"]["message"], "resource unavailable");
            assert!(!stale_read.to_string().contains(sentinel));
            assert!(!stale_read.to_string().contains(&project_db.display().to_string()));

            cancel.cancel();
            task.await.expect("daemon stopped");
        });
    });
}

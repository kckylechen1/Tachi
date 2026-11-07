//! Real HTTP binding from generic host context and optional MCP Roots.
#![allow(deprecated)] // Exercise both SDK compatibility channels for Roots.
use super::*;
use rmcp::{
    model::{CallToolRequestParams, ClientConfig, ListRootsResult, Root},
    ClientHandler, ServiceExt,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

fn save(id: &str) -> Value {
    json!({"action":"save", "id":id, "text":format!("WORKSPACEBINDING {id} exact text"),
        "summary":format!("WORKSPACEBINDING {id}"), "path":"/workspace-binding", "format":"json", "force":true})
}

fn global_count(server: &crate::MemoryServer) -> i64 {
    server
        .with_global_store_read(|store| {
            store
                .connection()
                .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
                .map_err(|error| error.to_string())
        })
        .unwrap()
}

fn assert_saved(server: &crate::MemoryServer, root: &Path, id: &str) {
    let project = server
        .resolve_existing_workspace_project(root.to_str().unwrap())
        .unwrap();
    assert!(
        server
            .with_named_project_store_read(&project, |store| {
                store.get(id).map_err(|error| error.to_string())
            })
            .unwrap()
            .is_some(),
        "{id} missing in {project}"
    );
}

fn file_root(path: &Path) -> Root {
    Root::new(url::Url::from_directory_path(path).unwrap().to_string())
}

fn roots_meta() -> Value {
    json!({"io.modelcontextprotocol/clientCapabilities":{"roots":{}}})
}

async fn modern_roots_call(caller: &Caller, id: i64, args: Value, roots: &[Root]) -> Value {
    let asked = caller
        .call_with_meta(id, "tachi_memory", args.clone(), roots_meta(), None)
        .await;
    assert_eq!(asked["result"]["resultType"], "input_required", "{asked}");
    assert_eq!(
        asked["result"]["inputRequests"]["tachi-workspace-roots"]["method"], "roots/list",
        "{asked}"
    );
    caller
        .call_with_meta(
            id + 1,
            "tachi_memory",
            args,
            roots_meta(),
            Some(json!({"tachi-workspace-roots":{"roots":roots}})),
        )
        .await
}

async fn modern_context_rpc(
    caller: &Caller,
    id: i64,
    method: &str,
    mut params: Value,
    extra: Value,
) -> Value {
    let mut headers = caller.headers.clone();
    headers.insert("mcp-protocol-version", "2026-07-28".parse().unwrap());
    headers.insert("mcp-method", method.parse().unwrap());
    if method == "resources/read" {
        headers.insert("mcp-name", params["uri"].as_str().unwrap().parse().unwrap());
    }
    let mut meta = json!({
        "io.modelcontextprotocol/protocolVersion":"2026-07-28",
        "io.modelcontextprotocol/clientCapabilities":{},
        "io.modelcontextprotocol/clientInfo":{"name":"binding-test","version":"1"}
    });
    meta.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    params["_meta"] = meta;
    let body = caller
        .client
        .post(&caller.url)
        .headers(headers)
        .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    parse_http_mcp_payload(&body, id)
}

#[test]
fn http_zcode_context_stays_tool_only_for_initialize_lists_and_resource_reads() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let root = temp.path().join("workspace");
        let listing = temp.path().join("listing");
        for path in [&root, &listing] {
            std::fs::create_dir(path).unwrap();
        }
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let observer = server.clone();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let zcode = json!({"com.zcode/request-context":{"workspace_path":root}});
            let (client, headers, init) =
                http_mcp_initialize(&daemon.url, http_headers(&[]), Some(zcode.clone())).await;
            assert!(init.get("error").is_none(), "{init}");
            assert!(!root.join(".tachi").exists());
            http_mcp_initialized(&client, &daemon.url, headers.clone()).await;
            let legacy = Caller {
                client,
                headers,
                url: daemon.url.clone(),
                modern: false,
            };
            assert_denied(
                legacy
                    .call(500, "tachi_memory", save("no-init-binding"), None)
                    .await,
            );

            let modern = Caller::new(&daemon.url, true, None, None).await;
            let list_context = json!({"com.zcode/request-context":{"workspace_path":listing}});
            let listed =
                modern_context_rpc(&modern, 501, "tools/list", json!({}), list_context.clone())
                    .await;
            assert!(listed.get("error").is_none(), "{listed}");
            assert!(!listing.join(".tachi").exists());
            let unavailable = modern_context_rpc(
                &modern,
                502,
                "resources/read",
                json!({"uri":"tachi-memory://v1/not-issued"}),
                list_context,
            )
            .await;
            assert!(unavailable.get("error").is_some(), "{unavailable}");
            assert!(!listing.join(".tachi").exists());

            payload(
                &modern
                    .call_with_meta(503, "tachi_memory", save("tool-only"), zcode.clone(), None)
                    .await,
            );
            let project = observer
                .resolve_existing_workspace_project(root.to_str().unwrap())
                .unwrap();
            let resource = observer
                .with_named_project_store_read(&project, |store| {
                    let entry = store
                        .get("tool-only")
                        .map_err(|error| error.to_string())?
                        .unwrap();
                    let source = crate::memory_resources::verified_project_source(
                        &observer, &project, store,
                    )
                    .unwrap();
                    Ok(crate::memory_resources::resource_link_for_entry(&source, &entry).unwrap())
                })
                .unwrap();
            let denied = modern_context_rpc(
                &modern,
                504,
                "resources/read",
                json!({"uri":resource.uri}),
                zcode,
            )
            .await;
            assert!(denied.get("error").is_some(), "{denied}");
            let admitted = modern_context_rpc(
                &modern,
                505,
                "resources/read",
                json!({"uri":resource.uri}),
                json!({"tachiProject":project}),
            )
            .await;
            assert!(admitted.get("error").is_none(), "{admitted}");
            assert_eq!(
                admitted["result"]["contents"][0]["text"],
                "WORKSPACEBINDING tool-only exact text"
            );
            assert_denied(
                legacy
                    .call(506, "tachi_memory", save("still-no-init-binding"), None)
                    .await,
            );
            assert_eq!(global_count(&observer), 0);
            assert!(observer.session_project().is_none());
            cancel.cancel();
            task.await.unwrap();
        });
    });
}

#[test]
fn http_generic_workspace_metadata_binds_paths_and_names_without_sticky_state() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for path in [&a, &b] {
            std::fs::create_dir(path).unwrap();
        }
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let observer = server.clone();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            for modern in [false, true] {
                let caller = Caller::new(&daemon.url, modern, None, None).await;
                for (id, root) in [(100, &a), (110, &b)] {
                    let key = format!("generic-{modern}-{id}");
                    payload(
                        &caller
                            .call_with_meta(
                                id,
                                "tachi_memory",
                                save(&key),
                                json!({"tachiWorkspaceRoot":root}),
                                None,
                            )
                            .await,
                    );
                    assert_saved(&observer, root, &key);
                }
                let project = observer
                    .resolve_existing_workspace_project(a.to_str().unwrap())
                    .unwrap();
                let key = format!("named-{modern}");
                payload(
                    &caller
                        .call_with_meta(
                            120,
                            "tachi_memory",
                            save(&key),
                            json!({"tachiProject":project}),
                            None,
                        )
                        .await,
                );
                assert_saved(&observer, &a, &key);
                assert_denied(
                    caller
                        .call_with_meta(
                            121,
                            "tachi_memory",
                            save("no-sticky-binding"),
                            json!({}),
                            None,
                        )
                        .await,
                );
            }
            assert_eq!(global_count(&observer), 0);
            assert!(observer.session_project().is_none());
            cancel.cancel();
            task.await.unwrap();
        });
    });
}

#[test]
fn http_conflicting_workspace_sources_refuse_before_registering_candidates() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            std::fs::create_dir(root).unwrap();
        }
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let observer = server.clone();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            for modern in [false, true] {
                let caller = Caller::new(&daemon.url, modern, None, None).await;
                for (i, meta) in [
                    json!({"tachiWorkspaceRoot":a,"com.zcode/request-context":{"workspace_path":b}}),
                    json!({"tachiWorkspaceRoot":a,"tachi.workspaceRoot":b}),
                    json!({"tachiProject":"unknown","com.zcode/request-context":{"workspace_path":a}}),
                    json!({"tachiWorkspaceRoot":null}),
                ].into_iter().enumerate() {
                    assert_denied(caller.call_with_meta(130 + i as i64, "tachi_memory", save("conflict"), meta, None).await);
                    assert!(!a.join(".tachi").exists());
                    assert!(!b.join(".tachi").exists());
                }
            }
            assert_eq!(global_count(&observer), 0);
            cancel.cancel(); task.await.unwrap();
        });
    });
}

#[test]
fn http_modern_roots_roundtrip_isolates_workspaces_and_defers_all_writes() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let _embedding = EnvRestore::set("TACHI_SEARCH_DISABLE_QUERY_EMBEDDING", "1");
        let global = temp.path().join("global/memory.db");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            std::fs::create_dir(root).unwrap();
        }
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let observer = server.clone();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let caller = Caller::new(&daemon.url, true, None, None).await;
            let asked = caller.call_with_meta(1, "tachi_memory", save("awaiting-roots"), roots_meta(), None).await;
            assert_eq!(asked["result"]["resultType"], "input_required", "{asked}");
            assert_eq!(global_count(&observer), 0);
            assert!(!a.join(".tachi").exists());
            assert!(!b.join(".tachi").exists());
            let roots_a = [file_root(&a)];
            let roots_b = [file_root(&b)];
            let (saved_a, saved_b) = tokio::join!(
                modern_roots_call(&caller, 10, save("root-alpha"), &roots_a),
                modern_roots_call(&caller, 20, save("root-beta"), &roots_b)
            );
            payload(&saved_a); payload(&saved_b);
            for (id, root, present, absent) in [(30, &a, "root-alpha", "root-beta"), (40, &b, "root-beta", "root-alpha"), (50, &a, "root-alpha", "root-beta")] {
                let found = payload(&modern_roots_call(&caller, id,
                    json!({"action":"search","query":"WORKSPACEBINDING","scope":"memory","format":"json"}), &[file_root(root)]).await);
                let rows = found["sections"][0]["rows"].to_string();
                assert!(rows.contains(present), "{found}");
                assert!(!rows.contains(absent), "{found}");
            }
            assert_saved(&observer, &a, "root-alpha");
            assert_saved(&observer, &b, "root-beta");
            assert_denied(caller.call_with_meta(60, "tachi_memory", save("no-context"), json!({}), None).await);
            assert!(observer.session_project().is_none());
            assert_eq!(global_count(&observer), 0);
            cancel.cancel(); task.await.unwrap();
        });
    });
}

#[test]
fn http_roots_reject_ambiguity_invalid_uris_and_empty_results_without_side_effects() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            std::fs::create_dir(root).unwrap();
        }
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let observer = server.clone();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let caller = Caller::new(&daemon.url, true, None, None).await;
            for (i, roots) in [
                vec![file_root(&a), file_root(&b)],
                vec![],
                vec![
                    file_root(&a),
                    Root::new("https://example.invalid/workspace"),
                ],
                vec![Root::new("file://other-host/workspace")],
                vec![Root::new(format!("{}?ambiguous=1", file_root(&a).uri))],
                vec![file_root(&a); 65],
            ]
            .into_iter()
            .enumerate()
            {
                assert_denied(
                    modern_roots_call(&caller, 200 + i as i64 * 2, save("rejected-roots"), &roots)
                        .await,
                );
                assert!(!a.join(".tachi").exists());
                assert!(!b.join(".tachi").exists());
            }
            for response in [
                json!({"wrong-key":{}}),
                json!({"tachi-workspace-roots":{"roots":"invalid"}}),
            ] {
                assert_denied(
                    caller
                        .call_with_meta(
                            230,
                            "tachi_memory",
                            save("bad-input"),
                            roots_meta(),
                            Some(response),
                        )
                        .await,
                );
            }
            assert_eq!(global_count(&observer), 0);
            cancel.cancel();
            task.await.unwrap();
        });
    });
}

fn git(primary: &Path, args: &[&str]) {
    let result = std::process::Command::new("git")
        .arg("-C")
        .arg(primary)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn http_roots_normalize_git_subdirectories_and_linked_worktrees_to_one_library() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let primary = temp.path().join("primary");
        let linked = temp.path().join("linked");
        std::fs::create_dir(&primary).unwrap();
        git(&primary, &["init", "-q"]);
        git(
            &primary,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "fixture",
            ],
        );
        git(
            &primary,
            &[
                "worktree",
                "add",
                "--detach",
                "-q",
                linked.to_str().unwrap(),
            ],
        );
        let nested = linked.join("src/component");
        std::fs::create_dir_all(&nested).unwrap();
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let observer = server.clone();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let caller = Caller::new(&daemon.url, true, None, None).await;
            payload(
                &modern_roots_call(
                    &caller,
                    300,
                    save("one-git-library"),
                    &[file_root(&primary), file_root(&linked), file_root(&nested)],
                )
                .await,
            );
            assert_saved(&observer, &primary, "one-git-library");
            assert!(!linked.join(".tachi").exists());
            assert!(!nested.join(".tachi").exists());
            assert_eq!(global_count(&observer), 0);
            cancel.cancel();
            task.await.unwrap();
        });
    });
}

#[derive(Clone)]
struct RootsClient {
    roots: Arc<Mutex<Vec<Root>>>,
    calls: Arc<AtomicUsize>,
    behavior: Arc<AtomicUsize>,
}

impl ClientHandler for RootsClient {
    fn get_info(&self) -> ClientConfig {
        let mut info = legacy_client_config();
        info.capabilities = rmcp::model::ClientCapabilities::builder()
            .enable_roots()
            .build();
        info
    }

    async fn list_roots(
        &self,
        _context: rmcp::service::RequestContext<rmcp::service::RoleClient>,
    ) -> Result<ListRootsResult, rmcp::ErrorData> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.behavior.load(Ordering::SeqCst) {
            1 => Err(rmcp::ErrorData::internal_error("fixture unavailable", None)),
            2 => {
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                Ok(ListRootsResult::default())
            }
            _ => Ok(ListRootsResult::new(self.roots.lock().unwrap().clone())),
        }
    }
}

fn tool_request(args: Value) -> CallToolRequestParams {
    CallToolRequestParams::new("tachi_memory").with_arguments(args.as_object().unwrap().clone())
}

#[test]
fn http_explicit_project_reads_skip_roots_without_authorizing_writes() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let target = temp.path().join("target");
        let a = temp.path().join("caller-a");
        let b = temp.path().join("caller-b");
        for path in [&target, &a, &b] {
            std::fs::create_dir(path).unwrap();
        }
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let observer = server.clone();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let modern = Caller::new(&daemon.url, true, None, None).await;
            payload(
                &modern
                    .call_with_meta(
                        600,
                        "tachi_memory",
                        save("explicit-read"),
                        json!({"tachiWorkspaceRoot":target}),
                        None,
                    )
                    .await,
            );
            let project = observer
                .resolve_existing_workspace_project(target.to_str().unwrap())
                .unwrap();
            let read =
                json!({"action":"get","id":"explicit-read","project":project,"format":"json"});
            let result = modern
                .call_with_meta(601, "tachi_memory", read.clone(), roots_meta(), None)
                .await;
            assert!(payload(&result)
                .to_string()
                .contains("WORKSPACEBINDING explicit-read"));

            let handler = RootsClient {
                roots: Arc::new(Mutex::new(vec![file_root(&a), file_root(&b)])),
                calls: Arc::new(AtomicUsize::new(0)),
                behavior: Arc::new(AtomicUsize::new(0)),
            };
            let client = handler
                .clone()
                .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
                    daemon.url.clone(),
                ))
                .await
                .unwrap();
            for behavior in [0, 1, 2] {
                handler.behavior.store(behavior, Ordering::SeqCst);
                let result = client.call_tool(tool_request(read.clone())).await.unwrap();
                assert_ne!(result.is_error, Some(true), "{result:?}");
                assert!(serde_json::to_string(&result)
                    .unwrap()
                    .contains("WORKSPACEBINDING explicit-read"));
            }
            assert_eq!(handler.calls.load(Ordering::SeqCst), 0);
            assert!(!a.join(".tachi").exists());
            assert!(!b.join(".tachi").exists());

            let mut write = save("not-authorized-by-project-argument");
            write["project"] = json!(project);
            assert_denied(modern_roots_call(&modern, 602, write.clone(), &[]).await);
            handler.behavior.store(1, Ordering::SeqCst);
            assert!(client.call_tool(tool_request(write)).await.is_err());
            assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
            assert!(observer
                .with_named_project_store_read(&project, |store| {
                    store
                        .get("not-authorized-by-project-argument")
                        .map_err(|error| error.to_string())
                })
                .unwrap()
                .is_none());
            assert_eq!(global_count(&observer), 0);
            client.cancel().await.unwrap();
            cancel.cancel();
            task.await.unwrap();
        });
    });
}

#[test]
fn http_legacy_roots_roundtrip_refreshes_workspace_and_bounds_failures() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            std::fs::create_dir(root).unwrap();
        }
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let observer = server.clone();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let handler = RootsClient {
                roots: Arc::new(Mutex::new(vec![file_root(&a)])),
                calls: Arc::new(AtomicUsize::new(0)),
                behavior: Arc::new(AtomicUsize::new(0)),
            };
            let client = handler
                .clone()
                .serve(rmcp::transport::StreamableHttpClientTransport::from_uri(
                    daemon.url.clone(),
                ))
                .await
                .unwrap();
            for (root, id) in [(&a, "legacy-a"), (&b, "legacy-b"), (&a, "legacy-a-again")] {
                *handler.roots.lock().unwrap() = vec![file_root(root)];
                let result = client.call_tool(tool_request(save(id))).await.unwrap();
                assert_ne!(result.is_error, Some(true), "{result:?}");
                assert_saved(&observer, root, id);
            }
            assert_eq!(handler.calls.load(Ordering::SeqCst), 3);
            *handler.roots.lock().unwrap() = vec![file_root(&a), file_root(&b)];
            assert!(client
                .call_tool(tool_request(save("ambiguous")))
                .await
                .is_err());
            handler.roots.lock().unwrap().clear();
            assert!(client
                .call_tool(tool_request(save("empty-roots")))
                .await
                .is_err());
            handler.behavior.store(1, Ordering::SeqCst);
            assert!(client
                .call_tool(tool_request(save("failed-roots")))
                .await
                .is_err());
            handler.behavior.store(2, Ordering::SeqCst);
            let refused = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                client.call_tool(tool_request(save("slow-roots"))),
            )
            .await
            .expect("server must bound Roots discovery");
            assert!(refused.is_err());
            assert_eq!(global_count(&observer), 0);
            assert!(observer.session_project().is_none());
            client.cancel().await.unwrap();
            cancel.cancel();
            task.await.unwrap();
        });
    });
}

#[test]
fn http_explicit_context_and_global_saves_skip_roots_discovery() {
    let temp = tempfile::tempdir().unwrap();
    with_tachi_home(temp.path(), || {
        let global = temp.path().join("global/memory.db");
        let root = temp.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        test_runtime().block_on(async {
            let server = crate::MemoryServer::new(global.clone(), None).unwrap();
            let observer = server.clone();
            let (daemon, cancel, task) = spawn_test_http_daemon(server, &global).await;
            let caller = Caller::new(&daemon.url, true, None, None).await;
            let mut explicit = roots_meta();
            explicit["tachiWorkspaceRoot"] = json!(root);
            payload(
                &caller
                    .call_with_meta(
                        400,
                        "tachi_memory",
                        save("explicit-context"),
                        explicit,
                        None,
                    )
                    .await,
            );
            assert_saved(&observer, &root, "explicit-context");
            let project = observer
                .resolve_existing_workspace_project(root.to_str().unwrap())
                .unwrap();
            let bound = Caller::new(&daemon.url, true, Some(&project), None).await;
            payload(
                &bound
                    .call_with_meta(
                        401,
                        "tachi_memory",
                        save("fixed-binding"),
                        roots_meta(),
                        None,
                    )
                    .await,
            );
            let mut global_args = save("explicit-global");
            global_args["storage_scope"] = json!("global");
            payload(
                &caller
                    .call_with_meta(402, "tachi_memory", global_args, roots_meta(), None)
                    .await,
            );
            assert_eq!(global_count(&observer), 1);
            let unrelated = caller
                .call_with_meta(403, "vault_connections", json!({}), roots_meta(), None)
                .await;
            assert_ne!(
                unrelated["result"]["resultType"], "input_required",
                "{unrelated}"
            );
            cancel.cancel();
            task.await.unwrap();
        });
    });
}

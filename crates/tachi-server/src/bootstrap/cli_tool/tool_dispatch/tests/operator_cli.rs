use super::*;
use rmcp::{
    model::*,
    service::{RequestContext, RoleServer},
    ServerHandler,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct ObservedServer {
    inner: crate::MemoryServer,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
}

impl ServerHandler for ObservedServer {
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        self.inner.supported_protocol_versions()
    }
    fn get_info(&self) -> ServerInfo {
        self.inner.get_info()
    }
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, rmcp::ErrorData> {
        self.inner.initialize(request, context).await
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let name = request.name.to_string();
        let response = self.inner.call_tool(request, context).await;
        let body = match &response {
            Ok(CallToolResponse::Complete(body)) => {
                serde_json::to_value(body).expect("response JSON")
            }
            Ok(_) => json!({"rpc_error":"non-complete response"}),
            Err(error) => json!({"rpc_error":error}),
        };
        self.calls
            .lock()
            .expect("call observations")
            .push((name, body));
        response
    }
}

#[test]
fn operator_cli_wiki_and_list_commands_reach_authorized_daemon_without_fallback() {
    let _lock = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = tempfile::tempdir().expect("home");
    let app_home = home.path().to_path_buf();
    let global = app_home.join("global/memory.db");
    let _home = EnvRestore::set_path("TACHI_HOME", &app_home);
    let _sigil = EnvRestore::remove("SIGIL_HOME");
    let _app = EnvRestore::remove("TACHI_APP_HOME");
    let _no_auto = EnvRestore::set("TACHI_DISABLE_AUTO_DAEMON", "1");
    let _daemon = EnvRestore::set("TACHI_DAEMON", "1");
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime").block_on(async {
        use rmcp::transport::streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        };
        let wiki = app_home.join("projects/wiki/memory.db");
        std::fs::create_dir_all(wiki.parent().expect("wiki parent")).expect("wiki directory");
        drop(crate::MemoryServer::new(global.clone(), Some(wiki)).expect("seed wiki schema"));
        let server = crate::MemoryServer::new(global.clone(), None).expect("server");
        server.set_tool_profile(Some(tachi_hub::ToolProfile::operate()));
        let token = uuid::Uuid::new_v4().simple().to_string();
        server.set_daemon_proxy_token(token.clone());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let config = StreamableHttpServerConfig::default()
            .with_legacy_session_mode(true)
            .with_stateless_protocol_metadata_required(true);
        let factory_server = server.clone();
        let service = StreamableHttpService::new(
            move || {
                Ok(ObservedServer {
                    inner: factory_server.clone_for_mcp_session(),
                    calls: recorded.clone(),
                })
            },
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let ct = CancellationToken::new();
        let stop = ct.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, axum::Router::new().nest_service("/mcp", service))
                .with_graceful_shutdown(stop.cancelled_owned())
                .await
                .expect("serve");
        });
        let receipt = crate::daemon_lock::scoped_daemon_pid_path(&app_home, &global);
        std::fs::create_dir_all(receipt.parent().expect("receipt parent")).expect("receipt dir");
        crate::utils::write_json_file_owner_only(&receipt, &json!({"pid":std::process::id(), "port":address.port(), "url":format!("http://{address}/mcp"), "global_db":global, "project_db":null, "version":env!("CARGO_PKG_VERSION"), "internal_proxy_token":token})).expect("fixture receipt");
        // An explicit daemon-wide Ops profile is not inherited by ordinary HTTP.
        // Even possession of its capability leaves generic CLI transport narrow.
        let daemon = crate::cli_client::DaemonInfo {
            url: format!("http://{address}/mcp"),
            global_db: Some(global.display().to_string()),
            project_db: None,
            version: Some(env!("CARGO_PKG_VERSION").into()),
            pid: Some(std::process::id() as i64),
            internal_proxy_token: Some(token.clone()),
        };
        for name in ["tachi_wiki", "list_memories"] {
            assert!(
                tachi_hub::tool_visible(name, server.active_tool_profile(), None),
                "the explicitly Ops local daemon owns this operator route"
            );
            let error =
                crate::cli_client::call_daemon_tool(&daemon, name, serde_json::Map::new(), None)
                    .await
                    .expect_err("ordinary generic transport must stay narrow");
            assert!(
                error.to_string().contains("tool not found"),
                "ordinary request denied at visibility boundary: {error}"
            );
        }
        use tachi_bootstrap::cli::Commands;
        let commands = [
            (
                "wiki write",
                "tachi_wiki",
                Commands::WikiWrite {
                    title: "operator regression".into(),
                    text: "operator CLI body".into(),
                    path: Some("/tests/operator-cli".into()),
                    topic: None,
                    summary: None,
                    keywords: vec![],
                    entities: vec![],
                    importance: None,
                    scope: Some("global".into()),
                    project: None,
                    domain: None,
                    force: true,
                },
            ),
            (
                "wiki search",
                "tachi_wiki",
                Commands::WikiSearch {
                    query: "operator regression".into(),
                    category: None,
                    top_k: 5,
                    project: None,
                },
            ),
            (
                "list",
                "list_memories",
                Commands::List {
                    path_prefix: "/tests/operator-cli".into(),
                    limit: 10,
                    include_archived: false,
                },
            ),
        ];
        let mut violations = Vec::new();
        for (label, expected_tool, command) in commands {
            calls.lock().expect("calls").clear();
            let result = crate::bootstrap::cli_tool::run_cli_command(
                command,
                &global,
                None,
                &app_home,
                &memcore::MigrationAuthority::Deny,
            )
            .await;
            if let Err(error) = result {
                violations.push(format!("{label} command failed: {error}"));
            }
            let observed = calls.lock().expect("calls");
            if observed.len() != 1
                || observed[0].0 != expected_tool
                || observed[0].1["isError"] == true
                || observed[0].1.get("rpc_error").is_some()
            {
                violations.push(format!("{label} did not complete on daemon (read fallback cannot count as success): {observed:?}"));
            }
        }
        ct.cancel();
        task.await.expect("daemon stopped");
        assert!(
            violations.is_empty(),
            "operator CLI daemon boundary failed:\n{}",
            violations.join("\n")
        );
    });
}

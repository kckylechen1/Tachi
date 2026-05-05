use super::*;
use crate::kanban::{CheckInboxParams, PostCardParams, UpdateCardParams};
use crate::vault_ops::{
    VaultGetParams, VaultInitParams, VaultListParams, VaultRemoveParams, VaultSetParams,
    VaultSetupRotationParams, VaultUnlockParams,
};
use memory_core::{AgentProjection, Pack};

fn ensure_test_env() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        std::env::set_var("VOYAGE_API_KEY", "test-voyage-key");
        std::env::set_var("SILICONFLOW_API_KEY", "test-siliconflow-key");
        std::env::set_var("SILICONFLOW_MODEL", "test-model");
        std::env::set_var("SUMMARY_MODEL", "test-summary-model");
        // Tests use a single global DB and seed paths across the canonical
        // layout (wiki, project, etc.). Disable path-routing validation so
        // those fixtures don't have to opt into cross-project routing.
        std::env::set_var("TACHI_DISABLE_PATH_VALIDATION", "1");
    });
}

fn home_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Tests that spawn real subprocesses sensitive to `HOME` (e.g. `npx`, which
/// reads `~/.npm` for cache and registry config) MUST acquire this lock.
/// Otherwise a concurrent `TempHomeGuard` (used by other tests) can repoint
/// `HOME` mid-spawn, breaking the subprocess in non-deterministic ways.
fn acquire_real_home_lock() -> std::sync::MutexGuard<'static, ()> {
    home_test_lock().lock().unwrap_or_else(|e| e.into_inner())
}

struct TempHomeGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    original_home: Option<std::ffi::OsString>,
    temp_home: std::path::PathBuf,
}

impl TempHomeGuard {
    fn new() -> Self {
        let guard = home_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let original_home = std::env::var_os("HOME");
        let temp_home =
            std::env::temp_dir().join(format!("tachi-test-home-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_home).expect("create temp home");
        std::env::set_var("HOME", &temp_home);
        Self {
            _guard: guard,
            original_home,
            temp_home,
        }
    }
}

impl Drop for TempHomeGuard {
    fn drop(&mut self) {
        if let Some(home) = self.original_home.as_ref() {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
        let _ = std::fs::remove_dir_all(&self.temp_home);
    }
}

fn make_server() -> MemoryServer {
    ensure_test_env();
    let db_path = std::env::temp_dir().join(format!(
        "memory-server-test-{}.sqlite",
        uuid::Uuid::new_v4()
    ));
    MemoryServer::new(db_path, None).expect("failed to create test server")
}

fn make_server_with_temp_home() -> (MemoryServer, TempHomeGuard) {
    ensure_test_env();
    let temp_home = TempHomeGuard::new();
    let global_db = temp_home.temp_home.join("global.sqlite");
    let server = MemoryServer::new(global_db, None).expect("failed to create test server");
    (server, temp_home)
}

fn shell_params(action: &str) -> TachiShellParams {
    TachiShellParams {
        action: action.to_string(),
        flow_id: None,
        task: None,
        title: None,
        agent: None,
        cwd: None,
        async_dispatch: false,
        project: None,
        state_filter: None,
        limit: None,
        notes: None,
        validation: Vec::new(),
        allowed_scope: Vec::new(),
    }
}

fn seed_wiki_project_entries(entries: Vec<MemoryEntry>) -> (MemoryServer, TempHomeGuard) {
    let (server, temp_home) = make_server_with_temp_home();
    let wiki_dir = temp_home.temp_home.join(".tachi/projects/wiki");
    std::fs::create_dir_all(&wiki_dir).expect("create wiki project dir");
    let wiki_db = wiki_dir.join("memory.db");
    let mut store =
        MemoryStore::open(wiki_db.to_str().expect("utf8 wiki db")).expect("open wiki project db");
    for entry in entries {
        store.upsert(&entry).expect("seed wiki project entry");
    }
    (server, temp_home)
}

fn make_entry(id: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        path: "/".to_string(),
        summary: "".to_string(),
        text: "test memory".to_string(),
        importance: 0.7,
        timestamp: Utc::now().to_rfc3339(),
        category: "fact".to_string(),
        topic: "".to_string(),
        keywords: Vec::new(),
        persons: Vec::new(),
        entities: Vec::new(),
        location: "".to_string(),
        source: "test".to_string(),
        scope: "general".to_string(),
        archived: false,
        access_count: 0,
        last_access: None,
        revision: 1,
        metadata: json!({}),
        vector: None,
        retention_policy: None,
        domain: None,
    }
}

fn make_test_tool(name: &str) -> rmcp::model::Tool {
    serde_json::from_value(json!({
        "name": name,
        "description": format!("tool {name}"),
        "inputSchema": {
            "type": "object",
            "additionalProperties": true,
        }
    }))
    .expect("failed to build test tool")
}

fn make_mcp_capability(id: &str, version: u32) -> HubCapability {
    let name = id.strip_prefix("mcp:").unwrap_or(id).to_string();
    HubCapability {
        id: id.to_string(),
        cap_type: "mcp".to_string(),
        name: name.clone(),
        version,
        description: format!("test capability {name}"),
        definition: json!({
            "transport": "stdio",
            "command": "/usr/bin/true",
            "args": [],
        })
        .to_string(),
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "gateway".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

async fn call_tool_via_server(
    server: MemoryServer,
    tool_name: &str,
    arguments: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    let mut params = rmcp::model::CallToolRequestParams::new(tool_name.to_string());
    if let Some(arguments) = arguments.filter(|args| !args.is_empty()) {
        params = params.with_arguments(arguments);
    }

    let request =
        rmcp::model::ClientRequest::CallToolRequest(rmcp::model::CallToolRequest::new(params));
    let (transport, mut receiver) =
        rmcp::transport::OneshotTransport::<rmcp::service::RoleServer>::new(
            rmcp::model::ClientJsonRpcMessage::request(request, rmcp::model::RequestId::Number(1)),
        );
    let service = rmcp::service::serve_directly(server, transport, None);

    let message = tokio::time::timeout(std::time::Duration::from_secs(3), receiver.recv())
        .await
        .expect("tool call timed out")
        .expect("tool call should yield one response");

    let quit_reason = service.waiting().await.expect("wait for oneshot service");
    assert!(
        matches!(quit_reason, rmcp::service::QuitReason::Closed),
        "oneshot service should close cleanly after one tool call"
    );

    match message {
        rmcp::model::ServerJsonRpcMessage::Response(response) => match response.result {
            rmcp::model::ServerResult::CallToolResult(result) => Ok(result),
            other => panic!("expected CallToolResult, got {other:?}"),
        },
        rmcp::model::ServerJsonRpcMessage::Error(error) => Err(error.error),
        other => panic!("expected tool response or error, got {other:?}"),
    }
}

fn make_skill_capability(
    id: &str,
    name: &str,
    description: &str,
    visibility: &str,
) -> HubCapability {
    HubCapability {
        id: id.to_string(),
        cap_type: "skill".to_string(),
        name: name.to_string(),
        version: 1,
        description: description.to_string(),
        definition: json!({
            "prompt": format!("Run skill {name}"),
            "content": format!("# {name}\n\n{description}"),
            "policy": {
                "visibility": visibility,
            },
            "inputSchema": {
                "type": "object",
                "properties": {
                    "input": {"type": "string"}
                }
            }
        })
        .to_string(),
        enabled: true,
        review_status: "approved".to_string(),
        health_status: "healthy".to_string(),
        last_error: None,
        last_success_at: None,
        last_failure_at: None,
        fail_streak: 0,
        active_version: None,
        exposure_mode: "direct".to_string(),
        uses: 0,
        successes: 0,
        failures: 0,
        avg_rating: 0.0,
        last_used: None,
        created_at: Utc::now().to_rfc3339(),
        updated_at: Utc::now().to_rfc3339(),
    }
}

mod bootstrap_tests;
mod dispatch_tests;
mod facade_tests;
mod handoff_tests;
mod hub_tests;
mod kanban_tests;
mod memory_tests;
mod merge_tests;
mod pack_tests;
mod profile_tests;
mod proxy_tests;
mod sandbox_tests;
mod skill_tests;
mod vault_tests;
mod vc_tests;
mod wiki_tests;

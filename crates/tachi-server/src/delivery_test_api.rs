//! Feature-gated friend surface for `tachi-delivery-tests` (#1831 Track T).
//!
//! This module is not part of the product API. It exposes controlled server
//! and delivery seam hooks for external delivery integration tests while
//! keeping them uncompiled in ordinary product builds.

use crate::server_state::MemoryServer;
use std::path::{Path, PathBuf};

/// Per-process run dir under `$TMPDIR/tachi-tests/run-<pid>-<uuid>/` with
/// automated GC for stale leftovers.
pub fn test_fixture_root() -> PathBuf {
    crate::utils::test_fixture_root()
}

/// Join `name` under [`test_fixture_root`].
pub fn test_fixture_path(name: impl AsRef<Path>) -> PathBuf {
    crate::utils::test_fixture_path(name)
}

pub struct DeliveryTestServer {
    inner: MemoryServer,
}

impl DeliveryTestServer {
    pub fn new_at(db_path: impl AsRef<Path>) -> Self {
        let server = MemoryServer::new_isolated_for_test(db_path.as_ref().to_path_buf(), None)
            .expect("test memory server");
        server.set_tool_profile(Some(tachi_hub::ToolProfile::coordinate()));
        server.set_published_daemon_mcp_url("http://127.0.0.1:6919/mcp".into());
        Self { inner: server }
    }

    pub fn with_global_store<T>(
        &self,
        f: impl FnOnce(&mut memcore::MemoryStore) -> Result<T, String>,
    ) -> Result<T, String> {
        self.inner.with_global_store(f)
    }

    pub fn set_work_claim_connection(
        &self,
        host_identity: Option<String>,
        connection_identity: String,
        admission_state: String,
    ) {
        self.inner
            .set_work_claim_connection(host_identity, connection_identity, admission_state);
    }

    pub fn set_session_project(&self, project: &str) {
        self.inner
            .set_session_identity(None, Some(project.to_owned()), None);
    }

    pub async fn handle_task_value(&self, value: serde_json::Value) -> Result<String, String> {
        let params: tachi_params::TachiTaskParams =
            serde_json::from_value(value).map_err(|e| e.to_string())?;
        self.inner
            .tachi_task(rmcp::handler::server::wrapper::Parameters(params))
            .await
    }

    pub fn handle_delivery(
        &self,
        params: tachi_params::TachiDeliveryParams,
    ) -> Result<String, String> {
        crate::delivery_ops::handle_tachi_delivery(&self.inner, params)
    }

    pub fn mint_delivery_for_managed(&self, row: &memcore::DispatchOutcomeRow, result_ref: String) {
        crate::delivery_ops::mint_delivery_for_managed_outcome(&self.inner, row, result_ref);
    }

    pub fn handle_delivery_value(&self, value: serde_json::Value) -> Result<String, String> {
        let params: tachi_params::TachiDeliveryParams =
            serde_json::from_value(value).map_err(|e| e.to_string())?;
        self.handle_delivery(params)
    }

    pub async fn handle_agent_eval(
        &self,
        params: tachi_params::TachiAgentEvalParams,
    ) -> Result<String, String> {
        crate::agent_eval::handle_agent_eval(&self.inner, params).await
    }

    pub async fn handle_agent_eval_value(
        &self,
        value: serde_json::Value,
    ) -> Result<String, String> {
        let params: tachi_params::TachiAgentEvalParams =
            serde_json::from_value(value).map_err(|e| e.to_string())?;
        self.handle_agent_eval(params).await
    }
}

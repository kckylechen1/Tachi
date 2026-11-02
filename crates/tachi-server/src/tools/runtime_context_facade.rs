use rmcp::handler::server::wrapper::Parameters;
use rmcp::{tool, tool_router};

use crate::foundry_runtime_ops::{
    handle_capture_session, handle_compact_context, handle_compact_rollup,
    handle_compact_session_memory, handle_recall_context, handle_section_build,
    COMPACT_CONTEXT_PERSIST_REFUSAL,
};
use crate::tool_params::{
    CaptureSessionParams, CompactContextParams, CompactRollupParams, CompactSessionMemoryParams,
    RecallContextParams, SectionBuildParams,
};
use crate::MemoryServer;

#[tool_router(router = runtime_context_tool_router, vis = "pub(crate)")]
impl MemoryServer {
    #[tool(
        description = "Recall structured memory context for an active agent turn. Returns ranked results plus a ready-to-inject prepend_context block."
    )]
    pub(crate) async fn recall_context(
        &self,
        Parameters(params): Parameters<RecallContextParams>,
    ) -> Result<String, String> {
        // JSON serialization turns nonfinite floats into null during forwarding.
        if params.min_score.is_some_and(|value| !value.is_finite()) {
            return Err("min_score must be finite".to_string());
        }
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "recall_context", &params).await?
        {
            return Ok(body);
        }
        handle_recall_context(self, params).await
    }

    #[tool(
        description = "Capture durable memories from a recent session window. Extracts structured memories, embeds them inside Tachi, and writes them to the configured store."
    )]
    pub(crate) async fn capture_session(
        &self,
        Parameters(params): Parameters<CaptureSessionParams>,
    ) -> Result<String, String> {
        if let Some(body) =
            crate::cli_client::maybe_forward_server_write(self, "capture_session", &params).await?
        {
            return Ok(body);
        }
        handle_capture_session(self, params).await
    }

    #[tool(
        description = "Compact a soon-to-be-evicted session window into a ready-to-inject context block. Designed for host runtimes that know when token pressure requires compaction. Never persists (persist=true is refused, #1099) — use compact_session_memory to persist a compacted window."
    )]
    pub(crate) async fn compact_context(
        &self,
        Parameters(params): Parameters<CompactContextParams>,
    ) -> Result<String, String> {
        // #1099/codex round-2: the persist=true refusal must be enforced here,
        // before any daemon forwarding. An already-running daemon (even one
        // reporting the same CARGO_PKG_VERSION, since this fix does not bump
        // the crate version) may still be serving the pre-#1099 binary and
        // would return the old nominal-success persist body, silently
        // bypassing the in-process refusal below. persist=true must never
        // reach the daemon at all.
        if params.persist {
            return Err(COMPACT_CONTEXT_PERSIST_REFUSAL.to_string());
        }
        if let Some(body) =
            crate::cli_client::maybe_forward_server_read(self, "compact_context", &params).await?
        {
            return Ok(body);
        }
        handle_compact_context(self, params).await
    }

    #[tool(
        description = "Render a structured context section with explicit layer and cache-boundary markers. Useful for host runtimes assembling static/session/live prompt sections."
    )]
    pub(crate) async fn section_build(
        &self,
        Parameters(params): Parameters<SectionBuildParams>,
    ) -> Result<String, String> {
        handle_section_build(self, params).await
    }

    #[tool(
        description = "Roll up multiple compacted session artifacts into a new compact summary block, preserving salient topics and durable signals for later reinjection."
    )]
    pub(crate) async fn compact_rollup(
        &self,
        Parameters(params): Parameters<CompactRollupParams>,
    ) -> Result<String, String> {
        handle_compact_rollup(self, params).await
    }

    #[tool(
        description = "Persist a compacted session artifact and its durable signals into Tachi memory, then optionally queue Foundry maintenance jobs."
    )]
    pub(crate) async fn compact_session_memory(
        &self,
        Parameters(params): Parameters<CompactSessionMemoryParams>,
    ) -> Result<String, String> {
        handle_compact_session_memory(self, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::time::Duration;

    #[tokio::test]
    async fn recall_context_rejects_nonfinite_before_daemon_forwarding() {
        let _lock = crate::utils::global_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let server = crate::tests::make_server();
        let _home =
            crate::test_support::EnvRestore::set_path("TACHI_HOME", &server.tachi_home_dir());
        let _daemon = crate::test_support::EnvRestore::set("TACHI_DAEMON", "0");
        let global_db = server.global_db_path_buf();
        let app_home = crate::path_utils::app_home_from_global_db(&global_db);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind daemon stand-in");
        let port = listener.local_addr().unwrap().port();
        let pid_path = crate::daemon_lock::scoped_daemon_pid_path(&app_home, &global_db);
        std::fs::create_dir_all(pid_path.parent().unwrap()).unwrap();
        std::fs::write(
            &pid_path,
            json!({
                "pid": std::process::id(),
                "port": port,
                "url": format!("http://127.0.0.1:{port}/mcp"),
                "global_db": global_db.display().to_string(),
                "project_db": null,
                "version": env!("CARGO_PKG_VERSION"),
            })
            .to_string(),
        )
        .unwrap();

        for min_score in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut params: RecallContextParams = serde_json::from_value(json!({
                "query": "",
                "include_wiki": false,
            }))
            .unwrap();
            params.min_score = Some(min_score);
            let result = tokio::select! {
                biased;
                connection = listener.accept() => {
                    connection.expect("accept daemon connection");
                    panic!("nonfinite min_score must not contact a daemon");
                }
                result = server.recall_context(Parameters(params)) => result,
            };
            assert_eq!(result.unwrap_err(), "min_score must be finite");
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );

        // Prove the same discovery path can reach the fixture after the refusal.
        let daemon = crate::cli_client::detect_daemon_for_global_db(&app_home, &global_db)
            .await
            .expect("daemon stand-in must be discoverable");
        assert!(crate::cli_client::daemon_version_matches(&daemon));
        assert!(crate::cli_client::daemon_matches_requested_dbs(
            &daemon, &global_db, None
        ));
        tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .expect("discovery must contact the daemon stand-in")
            .unwrap();
        std::fs::remove_file(pid_path).unwrap();

        let params = serde_json::from_value(json!({ "query": "", "min_score": 0 })).unwrap();
        let response = server.recall_context(Parameters(params)).await.unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["status"], "skipped");
        assert_eq!(response["reason"], "empty_query");
    }
}

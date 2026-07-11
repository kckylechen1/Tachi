//! `LaneRunner`: the seam between the curator's batch orchestration and the
//! *existing* dispatch machinery (#1002 mandate: "复用现有后端 lane 底座,不新
//! 造执行机"). Unit tests exercise `run_curator_batch` against a fake runner,
//! so no live GitHub call and no live lane spawn happen off this crate's
//! test suite. The live path, `DispatchLaneRunner`, wires straight into
//! `dispatch_ops::handle_tachi_dispatch` plus `complete_ops::handle_tachi_complete`,
//! reusing whichever dispatch profile the caller names — default
//! `codex_55_review`, the exact audit/verify lane card fit per #1002's
//! "审计/证伪类活按 lane card 路由".

use crate::server_state::MemoryServer;
use async_trait::async_trait;
use serde_json::json;

/// One lane's evidence report for a single re-verification packet.
#[derive(Debug, Clone)]
pub(crate) struct LaneOutcome {
    /// Raw evidence text (file:line-anchored findings) the lane produced.
    pub evidence_text: String,
    /// Tokens the lane self-reported spending (per existing `cost_tokens`
    /// self-report convention on `tachi_complete` — not independently
    /// measured; see #1002 scout notes).
    pub cost_tokens: u64,
    /// Which concrete lane id executed (echoed into the verdict row for
    /// audit — "which lane, how much, what did it decide").
    pub lane_id: String,
}

/// Abstraction over "hand a bounded re-verification packet to a lane and get
/// evidence back." Exists purely so `run_curator_batch`'s budget/skip/
/// idempotency logic is testable without a live GitHub token or subprocess
/// spawn — the trait boundary the #1002 scout recommended.
#[async_trait]
pub(crate) trait LaneRunner: Send + Sync {
    async fn reverify(
        &self,
        server: &MemoryServer,
        issue_ref: &str,
        packet: &str,
    ) -> Result<LaneOutcome, String>;
}

/// Live implementation: dispatches through the existing
/// `dispatch_ops::handle_tachi_dispatch` choke point using a named dispatch
/// profile (default `codex_55_review`), then records the eval via the
/// existing `complete_ops::handle_tachi_complete` pipeline so lane cards
/// (#534) keep evolving from curator runs exactly like any other dispatch.
pub(crate) struct DispatchLaneRunner {
    pub profile: String,
    pub cwd: Option<String>,
    pub timeout_secs: u64,
}

impl DispatchLaneRunner {
    pub fn new(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            cwd: None,
            timeout_secs: 600,
        }
    }
}

#[async_trait]
impl LaneRunner for DispatchLaneRunner {
    async fn reverify(
        &self,
        server: &MemoryServer,
        issue_ref: &str,
        packet: &str,
    ) -> Result<LaneOutcome, String> {
        use crate::tool_params::TachiDispatchParams;

        let dispatch_params = TachiDispatchParams {
            agent: None,
            profile: Some(self.profile.clone()),
            credential_profiles: Vec::new(),
            task: packet.to_string(),
            cwd: self.cwd.clone(),
            env_id: None,
            unmanaged_cwd: Some(true),
            skills: Vec::new(),
            context_query: None,
            model: None,
            timeout_secs: self.timeout_secs,
            permission_profile: None,
            allowed_tools: Vec::new(),
            completion_predicate: None,
            max_turns: None,
            sandbox: None,
            inject_tachi_mcp: None,
            inject_hub_mcps: None,
            command: Vec::new(),
            harness_transport: None,
            harness_server_url: None,
            project: None,
            stage: None,
            issue_ref: Some(issue_ref.to_string()),
            pr_ref: None,
            flow_id: None,
            tool_profile: None,
            auto_capability_bundle: None,
            mcp_access: None,
            allowed_mcp_servers: Vec::new(),
        };

        let raw = crate::dispatch_ops::handle_tachi_dispatch(server, dispatch_params).await?;
        let parsed: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| format!("parse dispatch response: {e}"))?;
        let dispatch_id = parsed
            .get("dispatch_id")
            .and_then(|d| d.as_str())
            .map(|s| s.to_string());

        // Curator dispatch is fire-and-report here: the caller of
        // `run_curator_batch` is responsible for polling/collecting the
        // dispatch result (mirrors the existing `clanker`/watchdog contract —
        // this module does not reinvent a second polling loop). The evidence
        // text returned here is a placeholder pointer, not the final
        // evidence — live wiring completes once the dispatch's `complete`
        // call lands (existing `complete_ops::handle_tachi_complete` path).
        Ok(LaneOutcome {
            evidence_text: json!({
                "dispatch_id": dispatch_id,
                "issue_ref": issue_ref,
                "profile": self.profile,
            })
            .to_string(),
            cost_tokens: 0,
            lane_id: self.profile.clone(),
        })
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Deterministic fake lane for unit tests: returns a pre-scripted
    /// outcome per `issue_ref`, or an error if the issue isn't scripted.
    pub(crate) struct FakeLaneRunner {
        pub outcomes: Mutex<HashMap<String, LaneOutcome>>,
        pub calls: Mutex<Vec<String>>,
    }

    impl FakeLaneRunner {
        pub fn new(outcomes: Vec<(&str, LaneOutcome)>) -> Self {
            Self {
                outcomes: Mutex::new(
                    outcomes
                        .into_iter()
                        .map(|(k, v)| (k.to_string(), v))
                        .collect(),
                ),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LaneRunner for FakeLaneRunner {
        async fn reverify(
            &self,
            _server: &MemoryServer,
            issue_ref: &str,
            _packet: &str,
        ) -> Result<LaneOutcome, String> {
            self.calls.lock().unwrap().push(issue_ref.to_string());
            self.outcomes
                .lock()
                .unwrap()
                .get(issue_ref)
                .cloned()
                .ok_or_else(|| format!("no scripted outcome for {issue_ref}"))
        }
    }
}

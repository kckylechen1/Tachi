//! `LaneRunner`: the seam between the curator's batch orchestration and the
//! *existing* dispatch machinery (#1002 mandate: "复用现有后端 lane 底座,不新
//! 造执行机"). Unit tests exercise `run_curator_batch` against a fake runner,
//! so no live GitHub call and no live lane spawn happen off this crate's
//! test suite. The live path, `DispatchLaneRunner`, wires straight into
//! `dispatch_ops::handle_tachi_dispatch`, then POLLS the dispatched lane to a
//! terminal state using the same primitives `tachi_task(action='wait')` uses
//! (`collect_run_task_for_server` + a terminal-state check), bounded by
//! `timeout_secs` — codex review Finding #1 (BROKEN, severity-max): the prior
//! version persisted `still_valid` from a `{dispatch_id, issue_ref, profile}`
//! placeholder before any lane result existed. This is fix option (a) from
//! that review: wait for real evidence using the existing wait machinery
//! instead of inventing a second polling loop.

use crate::server_state::MemoryServer;
use async_trait::async_trait;
use std::time::Duration;
use tokio::time::Instant;

/// Mirrors `tools::task_facade::is_terminal_task_state` — duplicated here
/// (rather than imported) because that fn is `pub(super)`-scoped to the
/// `tools` module; the 3-state contract is stable dispatch-ledger vocabulary,
/// not something curator_ops should reach across module boundaries for.
fn is_terminal_task_state(state: &str) -> bool {
    matches!(
        state,
        "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED"
    )
}

/// One lane's evidence report for a single re-verification packet. Produced
/// ONLY once the dispatched lane has reached a terminal state and left
/// non-empty evidence behind (`result.md` in its run dir) — see
/// `DispatchLaneRunner::reverify`. When no such evidence exists,
/// `ReverifyOutcome::NoEvidence` is returned instead; `LaneOutcome` can never
/// be constructed from a placeholder.
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

/// Result of asking a lane to re-verify one candidate. The `NoEvidence`
/// variant is Finding #1's structural fix surface: a timeout or a
/// terminal-but-empty result must be representable WITHOUT smuggling a real
/// verdict — `run_curator_batch` maps this straight to `pending_evidence` /
/// `lane_failed`, never to `still_valid`.
#[derive(Debug, Clone)]
pub(crate) enum ReverifyOutcome {
    Evidence(LaneOutcome),
    /// No terminal lane result was available before `timeout_secs` elapsed —
    /// the dispatch may still be running. Persistable only as
    /// `pending_evidence`.
    Timeout {
        lane_id: String,
    },
    /// The lane reached a terminal state but produced no usable evidence
    /// (dispatch call itself failed, or terminal state was
    /// failed/canceled with an empty/missing `result.md`). Persistable only
    /// as `lane_failed`.
    LaneFailed {
        lane_id: String,
        reason: String,
    },
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
    ) -> Result<ReverifyOutcome, String>;
}

/// Live implementation: dispatches through the existing
/// `dispatch_ops::handle_tachi_dispatch` choke point using a named dispatch
/// profile (default `codex_55_review`), then polls the SAME run-ledger
/// primitives `tachi_task(action='wait')` uses
/// (`dispatch_ops::collect_run_task_for_server`) until the dispatch reaches
/// a terminal state or `timeout_secs` elapses, and reads the lane's
/// `result.md` out of its run dir as the evidence payload. No second polling
/// loop is invented — this reuses the existing dispatch-ledger wait
/// machinery verbatim.
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

const POLL_INITIAL_DELAY: Duration = Duration::from_millis(500);
const POLL_MAX_DELAY: Duration = Duration::from_secs(10);

#[async_trait]
impl LaneRunner for DispatchLaneRunner {
    async fn reverify(
        &self,
        server: &MemoryServer,
        issue_ref: &str,
        packet: &str,
    ) -> Result<ReverifyOutcome, String> {
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
            .map(|s| s.to_string())
            .ok_or_else(|| "dispatch response missing dispatch_id".to_string())?;

        // Poll to terminal state using the exact same run-ledger primitive
        // `tachi_task(action='wait')` uses, bounded by the per-candidate
        // timeout that already exists on this runner — never a second,
        // parallel polling mechanism.
        let deadline = Instant::now() + Duration::from_secs(self.timeout_secs);
        let mut poll_delay = POLL_INITIAL_DELAY;
        loop {
            let task = crate::dispatch_ops::collect_run_task_for_server(server, &dispatch_id);
            if let Some(task) = &task {
                let state = task.get("state").and_then(|s| s.as_str()).unwrap_or("");
                if is_terminal_task_state(state) {
                    let run_dir = task.get("run_dir").and_then(|d| d.as_str());
                    let evidence_text = run_dir
                        .and_then(|dir| std::fs::read_to_string(format!("{dir}/result.md")).ok())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    return Ok(match evidence_text {
                        Some(evidence_text) if state == "TASK_STATE_COMPLETED" => {
                            ReverifyOutcome::Evidence(LaneOutcome {
                                evidence_text,
                                cost_tokens: 0,
                                lane_id: self.profile.clone(),
                            })
                        }
                        Some(evidence_text) => ReverifyOutcome::LaneFailed {
                            lane_id: self.profile.clone(),
                            reason: format!(
                                "dispatch {dispatch_id} reached terminal state {state} \
                                 (non-success); evidence on file: {evidence_text}"
                            ),
                        },
                        None => ReverifyOutcome::LaneFailed {
                            lane_id: self.profile.clone(),
                            reason: format!(
                                "dispatch {dispatch_id} reached terminal state {state} \
                                 with no result.md / empty evidence"
                            ),
                        },
                    });
                }
            }

            if Instant::now() >= deadline {
                return Ok(ReverifyOutcome::Timeout {
                    lane_id: self.profile.clone(),
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::time::sleep(poll_delay.min(remaining)).await;
            poll_delay = poll_delay.saturating_mul(2).min(POLL_MAX_DELAY);
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Deterministic fake lane for unit tests: returns a pre-scripted
    /// `ReverifyOutcome` per `issue_ref` (evidence, timeout, or lane-failed —
    /// exercising all three of Finding #1's persistable states), or an `Err`
    /// if the issue isn't scripted at all (a true infra failure, distinct
    /// from a scripted `LaneFailed`).
    pub(crate) struct FakeLaneRunner {
        pub outcomes: Mutex<HashMap<String, ReverifyOutcome>>,
        pub calls: Mutex<Vec<String>>,
    }

    impl FakeLaneRunner {
        pub fn new(outcomes: Vec<(&str, LaneOutcome)>) -> Self {
            Self::new_outcomes(
                outcomes
                    .into_iter()
                    .map(|(k, v)| (k, ReverifyOutcome::Evidence(v)))
                    .collect(),
            )
        }

        pub fn new_outcomes(outcomes: Vec<(&str, ReverifyOutcome)>) -> Self {
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
        ) -> Result<ReverifyOutcome, String> {
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

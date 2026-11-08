use crate::utils::lock_or_recover;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::time::Instant as IdleInstant;

// ─── MCP Client Connection Pool ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CircuitState {
    Closed,
    Open { until: Instant },
    HalfOpen { probe_in_flight: bool },
}

pub(crate) enum CircuitProbeDecision<'a> {
    Allowed(Option<HalfOpenProbeGuard<'a>>),
    Open,
    ProbeInProgress,
}

pub(crate) struct HalfOpenProbeGuard<'a> {
    pool: &'a McpClientPool,
    server_name: String,
}

impl Drop for HalfOpenProbeGuard<'_> {
    fn drop(&mut self) {
        let mut state = lock_or_recover(&self.pool.state, "mcp_pool.state");
        if let Some((CircuitState::HalfOpen { probe_in_flight }, _)) =
            state.circuits.get_mut(&self.server_name)
        {
            *probe_in_flight = false;
        }
    }
}

pub(crate) struct ChildConnection {
    /// The running MCP client service — we call peer() on this
    pub(super) client:
        rmcp::service::RunningService<rmcp::service::RoleClient, rmcp::model::ClientConfig>,
    activity: Arc<Mutex<ConnectionActivity>>,
}

#[derive(Debug)]
struct ConnectionActivity {
    in_flight: usize,
    idle_since: IdleInstant,
}

#[derive(Debug)]
pub(crate) struct ChildConnectionLease {
    peer: rmcp::service::Peer<rmcp::service::RoleClient>,
    activity: Arc<Mutex<ConnectionActivity>>,
}

impl ChildConnection {
    pub(super) fn new(
        client: rmcp::service::RunningService<rmcp::service::RoleClient, rmcp::model::ClientConfig>,
    ) -> Self {
        Self {
            client,
            activity: Arc::new(Mutex::new(ConnectionActivity {
                in_flight: 0,
                idle_since: IdleInstant::now(),
            })),
        }
    }

    pub(super) fn acquire(&self) -> ChildConnectionLease {
        lock_or_recover(&self.activity, "mcp_pool.activity").in_flight += 1;
        ChildConnectionLease {
            peer: self.client.peer().clone(),
            activity: Arc::clone(&self.activity),
        }
    }
}

impl ChildConnectionLease {
    pub(super) fn peer(&self) -> &rmcp::service::Peer<rmcp::service::RoleClient> {
        &self.peer
    }
}

impl Drop for ChildConnectionLease {
    fn drop(&mut self) {
        let mut activity = lock_or_recover(&self.activity, "mcp_pool.activity");
        activity.in_flight -= 1;
        if activity.in_flight == 0 {
            activity.idle_since = IdleInstant::now();
        }
    }
}

#[derive(Default)]
struct CallQuota {
    in_flight: Mutex<u64>,
    released: tokio::sync::Notify,
}

pub(crate) struct McpCallPermit {
    quota: Arc<CallQuota>,
}

impl CallQuota {
    async fn acquire(self: Arc<Self>, limit: u64) -> McpCallPermit {
        // A new limit changes admission, never the count of existing calls.
        loop {
            let released = self.released.notified();
            tokio::pin!(released);
            released.as_mut().enable();
            {
                let mut in_flight = lock_or_recover(&self.in_flight, "mcp_pool.quota");
                if *in_flight < limit.max(1) {
                    *in_flight += 1;
                    return McpCallPermit {
                        quota: Arc::clone(&self),
                    };
                }
            }
            released.await;
        }
    }
}

impl Drop for McpCallPermit {
    fn drop(&mut self) {
        *lock_or_recover(&self.quota.in_flight, "mcp_pool.quota") -= 1;
        self.quota.released.notify_waiters();
    }
}

pub(super) struct McpPoolState {
    pub(super) connections: HashMap<String, ChildConnection>,
    pub(super) circuits: HashMap<String, (CircuitState, u32)>,
    quotas: HashMap<String, Weak<CallQuota>>,
    pub(super) connecting_locks: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
}

impl McpPoolState {
    pub(super) fn remove_connection(&mut self, server_name: &str) -> bool {
        let before = self.connections.len();
        let prefix = format!("{server_name}#");
        self.connections
            .retain(|key, _| key != server_name && !key.starts_with(&prefix));
        before != self.connections.len()
    }
}

pub(crate) struct McpClientPool {
    pub(super) state: std::sync::Mutex<McpPoolState>,
    /// Idle TTL before auto-disconnect
    pub(super) idle_ttl: Duration,
}

impl McpClientPool {
    pub(crate) fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(McpPoolState {
                connections: HashMap::new(),
                circuits: HashMap::new(),
                quotas: HashMap::new(),
                connecting_locks: HashMap::new(),
            }),
            idle_ttl: Duration::from_secs(300),
        }
    }

    pub(super) fn acquire_circuit_probe<'a>(
        &'a self,
        server_name: &str,
        now: Instant,
    ) -> CircuitProbeDecision<'a> {
        let mut pool_state = lock_or_recover(&self.state, "mcp_pool.state");
        let Some((state, count)) = pool_state.circuits.get_mut(server_name) else {
            return CircuitProbeDecision::Allowed(None);
        };

        match state {
            CircuitState::Open { until } => {
                if now < *until {
                    return CircuitProbeDecision::Open;
                }
                *state = CircuitState::HalfOpen {
                    probe_in_flight: true,
                };
                *count = 0;
                CircuitProbeDecision::Allowed(Some(HalfOpenProbeGuard {
                    pool: self,
                    server_name: server_name.to_string(),
                }))
            }
            CircuitState::HalfOpen { probe_in_flight } => {
                if *probe_in_flight {
                    CircuitProbeDecision::ProbeInProgress
                } else {
                    *probe_in_flight = true;
                    CircuitProbeDecision::Allowed(Some(HalfOpenProbeGuard {
                        pool: self,
                        server_name: server_name.to_string(),
                    }))
                }
            }
            CircuitState::Closed => CircuitProbeDecision::Allowed(None),
        }
    }

    pub(crate) async fn acquire_call_permit(&self, resource: &str, limit: u64) -> McpCallPermit {
        let quota = {
            let mut state = lock_or_recover(&self.state, "mcp_pool.state");
            match state.quotas.get(resource).and_then(Weak::upgrade) {
                Some(quota) => quota,
                None => {
                    let quota = Arc::new(CallQuota::default());
                    state
                        .quotas
                        .insert(resource.to_owned(), Arc::downgrade(&quota));
                    quota
                }
            }
        };
        quota.acquire(limit).await
    }

    pub(crate) fn remove_idle_connections(&self, now: IdleInstant) -> Vec<String> {
        let mut state = lock_or_recover(&self.state, "mcp_pool.state");
        let stale: Vec<String> = state
            .connections
            .iter()
            .filter(|(_, connection)| {
                let activity = lock_or_recover(&connection.activity, "mcp_pool.activity");
                activity.in_flight == 0
                    && now.saturating_duration_since(activity.idle_since) > self.idle_ttl
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in &stale {
            state.connections.remove(name);
        }
        state.quotas.retain(|_, quota| quota.strong_count() > 0);
        stale
    }

    pub(crate) fn remove_connection(&self, server_name: &str) -> bool {
        lock_or_recover(&self.state, "mcp_pool.state").remove_connection(server_name)
    }

    #[cfg(test)]
    pub(crate) fn install_test_connection(
        &self,
        server_name: &str,
        client: rmcp::service::RunningService<rmcp::service::RoleClient, rmcp::model::ClientConfig>,
    ) {
        lock_or_recover(&self.state, "mcp_pool.state")
            .connections
            .insert(server_name.to_string(), ChildConnection::new(client));
    }
}

// ─── MCP Pool Proxy Methods on MemoryServer ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server_state::MemoryServer;
    use rmcp::ServiceExt;

    type CallArrival = (String, tokio::sync::oneshot::Sender<()>);

    struct HeldUpstream {
        entered: tokio::sync::mpsc::UnboundedSender<CallArrival>,
    }

    impl rmcp::ServerHandler for HeldUpstream {
        async fn call_tool(
            &self,
            request: rmcp::model::CallToolRequestParams,
            _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
        ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
            let (release, wait) = tokio::sync::oneshot::channel();
            self.entered
                .send((request.name.to_string(), release))
                .unwrap();
            let _ = wait.await;
            Ok(
                rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
                    "completed",
                )])
                .into(),
            )
        }
    }

    fn register_quota_cap(server: &MemoryServer, limit: u64) -> memcore::HubCapability {
        let mut cap = crate::tests::make_mcp_capability("mcp:quota-fixture", 1);
        cap.definition = serde_json::json!({
            "transport":"http", "url":"https://example.test/mcp",
            "max_concurrency":limit, "tool_timeout_ms":3_600_000,
            "discovery_status":"ready"
        })
        .to_string();
        if server.has_project_db() {
            server
                .with_project_store(|store| store.hub_register(&cap).map_err(|e| e.to_string()))
                .unwrap();
        } else {
            server
                .with_global_store(|store| store.hub_register(&cap).map_err(|e| e.to_string()))
                .unwrap();
        }
        cap
    }

    async fn install_held_upstream(
        server: &MemoryServer,
        cap: &memcore::HubCapability,
        entered: tokio::sync::mpsc::UnboundedSender<CallArrival>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let (server_io, client_io) = tokio::io::duplex(16384);
        let upstream = tokio::spawn(async move {
            let service = HeldUpstream { entered }.serve(server_io).await.unwrap();
            let _ = service.waiting().await;
        });
        let client = crate::mcp_connection::legacy_client_config()
            .serve(client_io)
            .await
            .unwrap();
        let key = server.mcp_connection_key(
            cap,
            None,
            server
                .get_effective_sandbox_policy(None, &cap.id)
                .0
                .as_ref(),
        );
        server.pool.install_test_connection(&key, client);
        (key, upstream)
    }

    async fn next_call(
        arrivals: &mut tokio::sync::mpsc::UnboundedReceiver<CallArrival>,
    ) -> CallArrival {
        tokio::time::timeout(Duration::from_secs(5), arrivals.recv())
            .await
            .expect("call must reach the upstream barrier")
            .expect("upstream must stay connected")
    }

    fn active_calls(pool: &McpClientPool, key: &str) -> usize {
        let state = lock_or_recover(&pool.state, "mcp_pool.state");
        let in_flight =
            lock_or_recover(&state.connections[key].activity, "mcp_pool.activity").in_flight;
        in_flight
    }

    #[tokio::test]
    async fn project_quota_cannot_be_reset_by_a_different_project_limit() {
        let (first, _) = crate::tests::make_server_with_project_fixture("quota-a");
        let (second_fixture, second_db) = crate::tests::make_server_with_project_fixture("quota-b");
        let mut second = MemoryServer::new_with_home_for_test(
            first.global_db_path_buf(),
            Some(second_db),
            second_fixture.tachi_home_dir(),
        )
        .unwrap();
        second.pool = Arc::clone(&first.pool);
        let first_cap = register_quota_cap(&first, 1);
        let second_cap = register_quota_cap(&second, 2);
        let (entered, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
        let (first_key, upstream_a) =
            install_held_upstream(&first, &first_cap, entered.clone()).await;
        let (_, upstream_b) = install_held_upstream(&second, &second_cap, entered).await;

        let first_call = tokio::spawn({
            let server = first.clone();
            async move {
                server
                    .proxy_call_internal("quota-fixture", "a1", None)
                    .await
            }
        });
        let (name, release_a1) = next_call(&mut arrivals).await;
        assert_eq!(name, "a1");
        let second_call = tokio::spawn(async move {
            second
                .proxy_call_internal("quota-fixture", "b1", None)
                .await
        });
        let (name, release_b1) = next_call(&mut arrivals).await;
        assert_eq!(name, "b1");

        let mut waiting = Box::pin(first.proxy_call_internal("quota-fixture", "a2", None));
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        assert_eq!(
            active_calls(&first.pool, &first_key),
            1,
            "a2 must still be queued"
        );

        release_a1.send(()).unwrap();
        first_call.await.unwrap().unwrap();
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        let (name, release_a2) = next_call(&mut arrivals).await;
        assert_eq!(name, "a2");
        release_a2.send(()).unwrap();
        waiting.await.unwrap();
        release_b1.send(()).unwrap();
        second_call.await.unwrap().unwrap();
        first.pool.remove_connection("quota-fixture");
        for upstream in [upstream_a, upstream_b] {
            upstream.abort();
            let _ = upstream.await;
        }
    }

    #[test]
    fn global_capability_keeps_one_quota_across_projects() {
        let (first, _) = crate::tests::make_server_with_project_fixture("quota-g1");
        let (second_fixture, second_db) =
            crate::tests::make_server_with_project_fixture("quota-g2");
        let second = MemoryServer::new_with_home_for_test(
            first.global_db_path_buf(),
            Some(second_db),
            second_fixture.tachi_home_dir(),
        )
        .unwrap();
        let global = crate::tests::make_mcp_capability("mcp:global-quota", 1);
        first
            .with_global_store(|store| store.hub_register(&global).map_err(|e| e.to_string()))
            .unwrap();
        assert_eq!(
            first.mcp_quota_key(&global.id),
            second.mcp_quota_key(&global.id),
            "a global capability must not get a fresh budget per project"
        );
        let project_cap = register_quota_cap(&first, 1);
        let mirrored = register_quota_cap(&second, 1);
        assert_eq!(project_cap.id, mirrored.id);
        assert_ne!(
            first.mcp_quota_key(&project_cap.id),
            second.mcp_quota_key(&project_cap.id),
            "project capabilities stay isolated per project"
        );
    }

    #[tokio::test]
    async fn reduced_quota_counts_calls_on_the_previous_connection_snapshot() {
        let server = crate::tests::make_server();
        let cap = register_quota_cap(&server, 2);
        let (entered, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
        let (old_key, old_upstream) = install_held_upstream(&server, &cap, entered.clone()).await;
        let mut calls = Vec::new();
        let mut releases = Vec::new();
        for name in ["old-1", "old-2"] {
            calls.push(tokio::spawn({
                let server = server.clone();
                async move {
                    server
                        .proxy_call_internal("quota-fixture", name, None)
                        .await
                }
            }));
            let (arrived, release) = next_call(&mut arrivals).await;
            assert_eq!(arrived, name);
            releases.push(release);
        }

        let cap = register_quota_cap(&server, 1);
        let (new_key, new_upstream) = install_held_upstream(&server, &cap, entered).await;
        assert_ne!(old_key, new_key);
        let mut waiting = Box::pin(server.proxy_call_internal("quota-fixture", "new", None));
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        assert_eq!(active_calls(&server.pool, &new_key), 0);
        releases.remove(0).send(()).unwrap();
        calls.remove(0).await.unwrap().unwrap();
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        assert_eq!(
            active_calls(&server.pool, &new_key),
            0,
            "one old call still fills the reduced quota"
        );

        releases.remove(0).send(()).unwrap();
        calls.remove(0).await.unwrap().unwrap();
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        let (name, release) = next_call(&mut arrivals).await;
        assert_eq!(name, "new");
        release.send(()).unwrap();
        waiting.await.unwrap();
        server.pool.remove_connection("quota-fixture");
        for upstream in [old_upstream, new_upstream] {
            upstream.abort();
            let _ = upstream.await;
        }
    }

    #[tokio::test]
    async fn quota_growth_and_cancelled_waiters_preserve_existing_permits() {
        let pool = McpClientPool::new();
        let first = pool.acquire_call_permit("resource", 1).await;
        let mut cancelled = Box::pin(pool.acquire_call_permit("resource", 1));
        assert!(futures::poll!(cancelled.as_mut()).is_pending());
        drop(cancelled);
        let second = pool.acquire_call_permit("resource", 2).await;
        let mut waiting = Box::pin(pool.acquire_call_permit("resource", 2));
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        drop(first);
        let third = waiting.await;
        let mut shrunk = Box::pin(pool.acquire_call_permit("resource", 1));
        assert!(futures::poll!(shrunk.as_mut()).is_pending());
        drop(second);
        assert!(futures::poll!(shrunk.as_mut()).is_pending());
        drop(third);
        drop(shrunk.await);
        pool.remove_idle_connections(IdleInstant::now());
        assert!(lock_or_recover(&pool.state, "mcp_pool.state")
            .quotas
            .is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn active_rpc_survives_idle_ttl_and_completion_starts_a_new_idle_period() {
        let server = crate::tests::make_server();
        let cap = register_quota_cap(&server, 1);
        let (entered, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
        let (key, upstream) = install_held_upstream(&server, &cap, entered).await;
        let call = tokio::spawn({
            let server = server.clone();
            async move {
                server
                    .proxy_call_internal("quota-fixture", "long", None)
                    .await
            }
        });
        let (_, release) = next_call(&mut arrivals).await;
        tokio::time::advance(Duration::from_secs(301)).await;
        assert!(server
            .pool
            .remove_idle_connections(IdleInstant::now())
            .is_empty());
        assert_eq!(active_calls(&server.pool, &key), 1);
        release.send(()).unwrap();
        call.await.unwrap().unwrap();
        tokio::time::advance(Duration::from_secs(299)).await;
        assert!(server
            .pool
            .remove_idle_connections(IdleInstant::now())
            .is_empty());
        tokio::time::advance(Duration::from_secs(2)).await;
        assert_eq!(
            server.pool.remove_idle_connections(IdleInstant::now()),
            vec![key]
        );
        upstream.abort();
        let _ = upstream.await;
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_and_timed_out_rpcs_release_connection_activity_and_resource_quota() {
        for cancel in [true, false] {
            let server = crate::tests::make_server();
            let cap = register_quota_cap(&server, 1);
            let (entered, mut arrivals) = tokio::sync::mpsc::unbounded_channel();
            let (key, upstream) = install_held_upstream(&server, &cap, entered).await;
            let call = tokio::spawn({
                let server = server.clone();
                async move {
                    server
                        .proxy_call_internal("quota-fixture", "unfinished", None)
                        .await
                }
            });
            let (_, release) = next_call(&mut arrivals).await;
            if cancel {
                tokio::time::advance(Duration::from_secs(301)).await;
                call.abort();
                assert!(call.await.unwrap_err().is_cancelled());
            } else {
                tokio::time::advance(Duration::from_secs(3601)).await;
                let error = call.await.unwrap().unwrap_err();
                assert!(error.to_string().contains("timed out"));
            }
            assert_eq!(active_calls(&server.pool, &key), 0);
            let resource = server.mcp_quota_key(&cap.id);
            let mut permit = Box::pin(server.pool.acquire_call_permit(&resource, 1));
            assert!(
                futures::poll!(permit.as_mut()).is_ready(),
                "cancel/timeout must return the quota seat"
            );
            drop(permit);
            assert!(server
                .pool
                .remove_idle_connections(IdleInstant::now())
                .is_empty());
            tokio::time::advance(Duration::from_secs(301)).await;
            assert_eq!(
                server.pool.remove_idle_connections(IdleInstant::now()),
                vec![key]
            );
            drop(release);
            upstream.abort();
            let _ = upstream.await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn old_connection_lease_cannot_release_a_replacement_connection() {
        let server = crate::tests::make_server();
        let cap = register_quota_cap(&server, 1);
        let (entered, _arrivals) = tokio::sync::mpsc::unbounded_channel();
        let (key, old_upstream) = install_held_upstream(&server, &cap, entered.clone()).await;
        let old_lease =
            lock_or_recover(&server.pool.state, "mcp_pool.state").connections[&key].acquire();
        server.pool.remove_connection("quota-fixture");
        let (_, new_upstream) = install_held_upstream(&server, &cap, entered).await;
        let new_lease =
            lock_or_recover(&server.pool.state, "mcp_pool.state").connections[&key].acquire();
        drop(old_lease);
        tokio::time::advance(Duration::from_secs(301)).await;
        assert!(server
            .pool
            .remove_idle_connections(IdleInstant::now())
            .is_empty());
        assert_eq!(active_calls(&server.pool, &key), 1);
        drop(new_lease);
        tokio::time::advance(Duration::from_secs(301)).await;
        assert_eq!(
            server.pool.remove_idle_connections(IdleInstant::now()),
            vec![key]
        );
        for upstream in [old_upstream, new_upstream] {
            upstream.abort();
            let _ = upstream.await;
        }
    }

    fn circuit_state(pool: &McpClientPool, server_name: &str) -> Option<CircuitState> {
        lock_or_recover(&pool.state, "mcp_pool.state")
            .circuits
            .get(server_name)
            .map(|(state, _)| *state)
    }

    #[test]
    fn half_open_circuit_allows_only_one_probe() {
        let pool = McpClientPool::new();
        let server_name = "demo";
        lock_or_recover(&pool.state, "mcp_pool.state")
            .circuits
            .insert(
                server_name.to_string(),
                (
                    CircuitState::Open {
                        until: Instant::now() - Duration::from_secs(1),
                    },
                    3,
                ),
            );

        let first = pool.acquire_circuit_probe(server_name, Instant::now());
        let first_guard = match first {
            CircuitProbeDecision::Allowed(Some(guard)) => guard,
            _ => panic!("expired open circuit should allow one half-open probe"),
        };
        assert!(matches!(
            circuit_state(&pool, server_name),
            Some(CircuitState::HalfOpen {
                probe_in_flight: true
            })
        ));

        assert!(matches!(
            pool.acquire_circuit_probe(server_name, Instant::now()),
            CircuitProbeDecision::ProbeInProgress
        ));

        drop(first_guard);
        assert!(matches!(
            circuit_state(&pool, server_name),
            Some(CircuitState::HalfOpen {
                probe_in_flight: false
            })
        ));

        let second = pool.acquire_circuit_probe(server_name, Instant::now());
        let _second_guard = match second {
            CircuitProbeDecision::Allowed(Some(guard)) => guard,
            _ => panic!("released half-open circuit should allow the next probe"),
        };
    }

    #[test]
    fn half_open_probe_guard_preserves_terminal_circuit_state() {
        let pool = McpClientPool::new();
        let server_name = "demo";
        lock_or_recover(&pool.state, "mcp_pool.state")
            .circuits
            .insert(
                server_name.to_string(),
                (
                    CircuitState::HalfOpen {
                        probe_in_flight: false,
                    },
                    0,
                ),
            );

        let probe = pool.acquire_circuit_probe(server_name, Instant::now());
        let probe_guard = match probe {
            CircuitProbeDecision::Allowed(Some(guard)) => guard,
            _ => panic!("half-open circuit should allow a probe when none is active"),
        };
        lock_or_recover(&pool.state, "mcp_pool.state")
            .circuits
            .insert(server_name.to_string(), (CircuitState::Closed, 0));

        drop(probe_guard);

        assert_eq!(
            circuit_state(&pool, server_name),
            Some(CircuitState::Closed)
        );
    }

    #[tokio::test]
    async fn circuit_open_evicts_all_server_snapshots() {
        struct Upstream;
        impl rmcp::ServerHandler for Upstream {}

        let server = crate::tests::make_server();
        let cap = crate::tests::make_mcp_capability("mcp:demo", 1);
        let first = server.mcp_connection_key(&cap, None, None);
        let second = server.mcp_connection_key(&cap, Some("vc:other-policy"), None);
        let other = server.mcp_connection_key(
            &crate::tests::make_mcp_capability("mcp:demo-other", 1),
            None,
            None,
        );
        assert_ne!(first, second, "fixture must contain different snapshots");

        for initial in [
            CircuitState::Closed,
            CircuitState::HalfOpen {
                probe_in_flight: true,
            },
        ] {
            let mut upstreams = Vec::new();
            for key in ["demo", first.as_str(), second.as_str(), other.as_str()] {
                let (server_io, client_io) = tokio::io::duplex(4096);
                upstreams.push(tokio::spawn(async move {
                    let service = Upstream.serve(server_io).await.unwrap();
                    let _ = service.waiting().await;
                }));
                let client = crate::mcp_connection::legacy_client_config()
                    .serve(client_io)
                    .await
                    .unwrap();
                server.pool.install_test_connection(key, client);
            }
            lock_or_recover(&server.pool.state, "mcp_pool.state")
                .circuits
                .insert("demo".into(), (initial, 0));

            if initial == CircuitState::Closed {
                server.record_circuit_failure("demo");
                server.record_circuit_failure("demo");
                assert_eq!(
                    lock_or_recover(&server.pool.state, "mcp_pool.state")
                        .connections
                        .len(),
                    4,
                    "failures below the threshold must preserve connections"
                );
            }
            server.record_circuit_failure("demo");
            {
                let state = lock_or_recover(&server.pool.state, "mcp_pool.state");
                assert!(matches!(
                    state.circuits["demo"].0,
                    CircuitState::Open { .. }
                ));
                assert!(state.connections.contains_key(&other));
                assert_eq!(
                    state.connections.len(),
                    1,
                    "opening from {initial:?} must evict every demo snapshot"
                );
            }
            server.pool.remove_connection("demo-other");
            for upstream in upstreams {
                upstream.abort();
                let _ = upstream.await;
            }
        }
    }
}

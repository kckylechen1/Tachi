use crate::utils::lock_or_recover;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    pub(super) client: rmcp::service::RunningService<rmcp::service::RoleClient, ()>,
    pub(super) last_used: Instant,
}

pub(super) struct McpPoolState {
    pub(super) connections: HashMap<String, ChildConnection>,
    pub(super) circuits: HashMap<String, (CircuitState, u32)>,
    pub(super) semaphores: HashMap<String, (Arc<tokio::sync::Semaphore>, usize)>,
    pub(super) connecting_locks: HashMap<String, Arc<tokio::sync::Mutex<()>>>,
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
                semaphores: HashMap::new(),
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

    pub(crate) fn remove_idle_connections(&self, now: Instant) -> Vec<String> {
        let mut state = lock_or_recover(&self.state, "mcp_pool.state");
        let stale: Vec<String> = state
            .connections
            .iter()
            .filter(|(_, connection)| now.duration_since(connection.last_used) > self.idle_ttl)
            .map(|(name, _)| name.clone())
            .collect();
        for name in &stale {
            state.connections.remove(name);
        }
        stale
    }

    pub(crate) fn remove_connection(&self, server_name: &str) -> bool {
        lock_or_recover(&self.state, "mcp_pool.state")
            .connections
            .remove(server_name)
            .is_some()
    }
}

// ─── MCP Pool Proxy Methods on MemoryServer ──────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

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
}

use super::super::*;

pub(in crate::dispatch_profile) fn simulate_route_policy(
    policy: &str,
    performance_matrix: &[AgentPerformanceMatrixRow],
    focus: Option<&DispatchRisk>,
) -> RouteSimulationSummary {
    tachi_dispatch::simulate_route_policy(
        policy,
        &route_performance_rows(performance_matrix),
        focus,
    )
}

pub(super) fn route_simulation_caveats(
    rows: &[EvalRow],
    performance_matrix: &[AgentPerformanceMatrixRow],
) -> Vec<String> {
    tachi_dispatch::route_simulation_caveats(
        rows.len(),
        &route_performance_rows(performance_matrix),
    )
}

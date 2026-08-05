mod rules;
mod simulation;

// #1426: the route-policy proposal/review/apply/simulate HANDLERS moved to
// `crate::tune_ops::route_policy` with the `tachi_tune` surface. What stays
// here is the policy kernel they call back into: rule loadout resolution and
// the pure route simulation.
pub(super) use rules::load_route_policy_rule_loadout;
pub(crate) use simulation::{route_simulation_caveats, simulate_route_policy};

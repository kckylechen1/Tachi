mod simulation;

// #1426: the route-policy proposal/review/apply/simulate HANDLERS moved to
// `crate::tune_ops::route_policy` with the `tachi_tune` surface. What stays
// here is the policy kernel it calls back into: the pure route simulation.
//
// tachi#1675 BUG-8: `load_route_policy_rule_loadout` (rule-loadout
// resolution: one `list_state(ROUTE_POLICY_RULE_NS)` read + a
// `tachi_dispatch::build_route_policy_rule_loadout` call) used to live in
// this module's `rules` submodule. `dispatch_profile::routing::recommendation`
// was its only production caller, and that caller now inlines the same
// read-once sequence itself so a single snapshot can feed both the
// candidate-scoring loadout AND `policy_source_revision`'s content hash
// (a second, differently-timed internal read inside the old helper was a
// TOCTOU window). With no production caller left, the wrapper was deleted
// outright (`policy/rules.rs` removed) rather than kept as a
// `#[cfg(test)]`-gated relic — its former unit-test coverage
// (`build_route_policy_rule_loadout_classifies_persisted_rules`, in
// `dispatch_profile::tests::route_policy`) now drives
// `tachi_dispatch::build_route_policy_rule_loadout` directly, pinning the
// same risk-classification contract through the actual production entry
// point.
pub(crate) use simulation::{route_simulation_caveats, simulate_route_policy};

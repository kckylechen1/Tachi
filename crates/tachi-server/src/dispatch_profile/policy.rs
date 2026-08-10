mod rules;
mod simulation;

// #1426: the route-policy proposal/review/apply/simulate HANDLERS moved to
// `crate::tune_ops::route_policy` with the `tachi_tune` surface. What stays
// here is the policy kernel they call back into: rule loadout resolution and
// the pure route simulation.
//
// tachi#1675 BUG-8: `recommendation.rs` (the last production caller) now
// inlines the same read-once-build-loadout-and-hash sequence itself, so a
// single `list_state` snapshot can feed both the scoring loadout AND
// `policy_source_revision`'s hash (this function's own internal read would
// otherwise be a second, differently-timed read — the TOCTOU this fix
// closes). `load_route_policy_rule_loadout` has no remaining production
// caller (triggers a `dead_code`/`unused_imports` warning in a non-test
// build, left as-is rather than `#[cfg(test)]`-gated or deleted — out of
// this fix's scope) — left in place because its own dedicated unit test
// (`load_route_policy_rule_loadout_classifies_persisted_rules`) still
// documents the risk-classification contract
// `build_route_policy_rule_loadout` implements, useful reference for the
// dispatch-spine follow-up PR that will need the equivalent sequence.
pub(super) use rules::load_route_policy_rule_loadout;
pub(crate) use simulation::{route_simulation_caveats, simulate_route_policy};

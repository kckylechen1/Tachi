use super::*;

mod apply;
mod recommendation;
mod risk;

#[cfg(test)]
pub(crate) use self::apply::resolve_and_apply_dispatch_profile;
pub(crate) use self::apply::resolve_and_apply_dispatch_profile_for_server;
pub(crate) use self::recommendation::handle_dispatch_recommendation;
// Test-only re-export (`dispatch_profile/tests` glob-imports via parent).
#[cfg(test)]
pub(super) use self::recommendation::recommended_transport_for_profile;
pub(super) use self::risk::classify_dispatch_risk;
#[cfg(test)]
pub(in crate::dispatch_profile) use tachi_dispatch::route_eval_rows;
pub(in crate::dispatch_profile) use tachi_dispatch::route_performance_rows;

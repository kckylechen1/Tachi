mod apply;
mod handlers;

pub(crate) use apply::handle_route_policy_apply;
pub(crate) use handlers::{
    handle_route_policy_proposals, handle_route_policy_review, handle_route_simulation,
};
// tachi#1675 PR1 Seam A: `handle_dispatch_recommendation` needs the SAME
// content-bearing policy snapshot hash `handle_route_policy_proposals`
// already uses (not a reinvented one) to stamp
// `route_recommendations.policy_source_revision`.
pub(crate) use handlers::{content_digest_hex, route_policy_source_revision};

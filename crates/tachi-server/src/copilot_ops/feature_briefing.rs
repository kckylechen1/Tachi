mod board;
mod compact;
mod dispatch;
mod docs;
mod handlers;
mod markdown;
mod stage;

pub(crate) use handlers::{feature_briefing, handle_tachi_feature_briefing};

#[cfg(test)]
pub(super) use board::value_contains_any;

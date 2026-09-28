// chat_lanes.rs — chat/completion lane API calls on LlmClient

mod claude_cli;
mod generators;
mod lane_calls;

// Test seam: the writer tests drive the exact production usage-persist
// boundary in `lane_calls` without a chat lane call.
#[cfg(test)]
pub(in crate::llm) use self::lane_calls::persist_llm_usage_blocking;

pub use claude_cli::ReasoningOutcome;

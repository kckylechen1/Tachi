mod agent;
mod coerce;
mod dlq;
mod facade;
mod foundry;
mod gh;
mod hub;
mod memory;
mod peer;
mod project_db;
mod recall_evidence;
mod refinery;
mod sandbox;

pub use agent::*;
pub use dlq::*;
pub use facade::*;
pub use foundry::*;
pub use gh::*;
pub use hub::*;
pub use memory::*;
pub use peer::*;
pub use project_db::*;
pub use recall_evidence::*;
pub use refinery::*;
pub use sandbox::*;

/// Public value-level coercion helpers shared with callers outside this
/// crate (e.g. the RPC transport layer, see #970) that need the same
/// lenient Null/Number/String coercion `TachiTaskParams` and friends use
/// via `deserialize_with`, but as a plain function on a `&serde_json::Value`
/// rather than a serde deserializer. Only the lenient, non-erroring helper
/// is exported; the strict `deserialize_with` functions stay crate-private
/// since they're serde plumbing, not a public API surface.
pub use coerce::opt_u64_from_value;

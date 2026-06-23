use serde_json::Value;

pub(crate) struct ResolvedCallTarget {
    pub requested_id: String,
    pub resolved_id: String,
    pub requested_kind: String,
    pub resolution: Value,
}

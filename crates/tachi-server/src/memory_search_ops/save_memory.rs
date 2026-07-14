mod enrichment;
mod entry;
mod error;
mod handler;
mod persist;
mod remember;
mod response;
mod validation;
pub(crate) mod write_affinity;

pub(crate) use handler::handle_save_memory;
pub(crate) use remember::handle_remember;

/// Stable internal API for `complete_ops`: persist the eval record produced by
/// `tachi_complete` through the standard save pipeline (scrub, validate,
/// enrich). Complete records must go through this path so they benefit from the
/// same secret-scrubbing and dedup guards as every other memory entry.
pub(crate) async fn save_eval_memory(
    server: &crate::MemoryServer,
    params: crate::tool_params::SaveMemoryParams,
) -> Result<String, String> {
    handler::handle_save_memory(server, params).await
}

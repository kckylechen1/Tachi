mod enrichment;
mod entry;
mod error;
mod handler;
mod persist;
mod remember;
mod response;
mod validation;
pub(crate) mod write_affinity;

pub(crate) use handler::{
    handle_save_memory, handle_save_memory_from_caller, handle_save_memory_with_references,
};
pub(crate) use handler::{
    handle_save_memory_with_authorized_reference_mutations,
    handle_save_memory_with_wiki_projection,
};
#[cfg(test)]
pub(crate) use handler::{
    install_pre_upsert_barrier, install_pre_upsert_identity_barrier,
    install_pre_upsert_path_barrier, install_pre_upsert_pause,
};
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

/// Internal server-authorized reference path. The memcore value validates
/// shape only; this boundary is responsible for deciding the producer is
/// allowed to mutate reserved provenance metadata.
pub(crate) async fn save_eval_memory_with_authorized_reference_mutations(
    server: &crate::MemoryServer,
    params: crate::tool_params::SaveMemoryParams,
    mutations: Vec<memcore::db::ValidatedReferenceMutation>,
) -> Result<String, String> {
    handler::handle_save_memory_with_authorized_reference_mutations(server, params, mutations).await
}

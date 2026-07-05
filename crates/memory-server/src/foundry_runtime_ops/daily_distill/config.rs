pub use tachi_foundry::scrub_agent_noise;

pub(crate) use tachi_foundry::{
    resolve_batch_size, resolve_candidate_scan_limit, resolve_distill_backend,
    resolve_processed_scan_limit, DistillBackend,
};

#[cfg(test)]
pub(crate) use tachi_foundry::{
    DEFAULT_CANDIDATE_SCAN_LIMIT, DEFAULT_GROUPS_PER_BATCH, DEFAULT_PROCESSED_SCAN_LIMIT,
};

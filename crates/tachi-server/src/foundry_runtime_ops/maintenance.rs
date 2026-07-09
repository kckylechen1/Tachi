mod distill_job;
mod enqueue;
mod forget;
mod neighborhood;
mod store;
mod worker;

#[cfg(test)]
mod phase1_tests;

/// Outcome of a memory-distill job. Carries a structured skip reason so
/// foundry_jobs.metadata.terminal_reason answers "why didn't this run?" instead
/// of the previous opaque "worker reported no-op". The reason is persisted under
/// the metadata `$.terminal_reason` key by update_foundry_job_status_with_reason.
pub(crate) enum DistillOutcome {
    Wrote,
    Skipped(String),
}

/// All source memories filtered out (archived or already a distill output).
pub(crate) const SKIP_NO_SOURCE_ENTRIES: &str = "no_source_entries";
/// No coherent topic/entity bucket among the source memories.
pub(crate) const SKIP_NO_COHERENT_BUCKET: &str = "no_coherent_bucket";
/// LLM returned an empty payload (post-trim).
pub(crate) const SKIP_EMPTY_LLM_OUTPUT: &str = "empty_llm_output";

#[cfg(test)]
pub(super) use enqueue::capture_maintenance_specs;
pub(super) use enqueue::enqueue_capture_maintenance_jobs;
pub(crate) use enqueue::enqueue_foundry_capture_maintenance;
#[cfg(test)]
pub(super) use store::memory_claim_signature;
pub(super) use store::with_foundry_store_read;
pub(crate) use worker::run_foundry_maintenance_worker;

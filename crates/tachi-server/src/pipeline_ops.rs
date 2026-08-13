mod audit;
mod auto_ingest;
pub(crate) mod helpers;
mod ingest;
mod status;
mod sync;

pub(crate) use auto_ingest::replay_pending_auto_ingest_once;
pub(crate) use auto_ingest::{
    run_auto_ingest_replay_consumer, run_staged_auto_ingest, stage_auto_ingest_from_mcp,
};
#[cfg(test)]
pub(crate) use auto_ingest::{validate_admitted_ingest_bounds_for_test, StagedAutoIngest};
pub(crate) use helpers::calculate_promotion_score;
#[cfg(test)]
pub(crate) use ingest::force_next_admitted_enrichment_ownership_loss_for_test;
pub(crate) use ingest::{handle_extract_facts, handle_ingest_event};
pub(crate) use status::handle_get_pipeline_status;
pub(crate) use sync::handle_sync_memories;

use super::*;
use crate::tests::make_server_with_project_fixture;

fn assert_project_memory_destination(server: &crate::MemoryServer, id: &str) {
    assert!(server
        .with_project_store_read(|store| store.get(id).map_err(|error| error.to_string()))
        .expect("read saved project memory")
        .is_some());
    assert!(server
        .with_global_store_read(|store| store.get(id).map_err(|error| error.to_string()))
        .expect("inspect global memory absence")
        .is_none());
}

mod auto_link;
mod auto_link_latency_w5;
mod continuity_event;
mod dedup;
mod legacy_fields;
mod metadata_validation;
mod provenance;
mod recall_cache_invalidation;
mod redaction_scrub;

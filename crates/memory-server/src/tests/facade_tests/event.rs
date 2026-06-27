use super::*;

fn tachi_event_params(action: &str) -> TachiEventParams {
    TachiEventParams {
        action: action.to_string(),
        format: None,
        id: None,
        source_repo: None,
        adapter: None,
        project: None,
        domain: None,
        session_id: None,
        actor: None,
        event_type: None,
        authority: None,
        effects: Vec::new(),
        projection_hints: Vec::new(),
        payload: None,
        provenance: None,
        created_at: None,
        limit: 20,
        path_prefix: None,
        dry_run: false,
    }
}

mod context;
mod emit_query;
mod label_eval;
mod metrics;
mod project;

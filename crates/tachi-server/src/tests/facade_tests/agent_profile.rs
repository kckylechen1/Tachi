use super::*;

fn profile_params(action: &str) -> TachiProfileParams {
    TachiProfileParams {
        action: action.to_string(),
        agent_id: Some("codex".to_string()),
        display_name: Some("Codex".to_string()),
        target: None,
        targets: Vec::new(),
        documents: Vec::new(),
        document_paths: Vec::new(),
        pack: None,
        project: None,
        role: None,
        session_kind: None,
        include_private_user: false,
        include_continuity: false,
        max_chars: 4_000,
        dry_run: true,
    }
}

mod context;
mod guards;
mod import_render;

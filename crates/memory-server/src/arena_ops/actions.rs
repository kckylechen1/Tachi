use crate::{MemoryServer, TachiArenaParams};
use chrono::Utc;
use serde_json::{json, Value};

use super::dispatch_bridge::{completion_draft_for_mission, dispatch_params_for_mission};
use super::lane::{harness_lane, tracked_worker_prompt};
use super::render::{render_arena_md, render_prompt_md, render_summary_md};
use super::state::{
    active_state, arena_dir, arena_root, dispatch_response_summary, mission_dir, mission_statuses,
    new_arena_id, new_mission_id, nonempty_file, read_arena_artifact, read_json_file,
    read_linked_dispatch_result, refresh_linked_dispatch_fields, update_mission_status,
    validate_arena_id, validate_mission_id, ArenaArtifactRead,
};

mod board;
mod collect;
mod lifecycle;
mod open;
mod spawn;

use board::{handle_board, refresh_board};
use collect::handle_collect;
use lifecycle::{handle_abort, handle_close, handle_reap};
use open::handle_open;
use spawn::handle_spawn;

pub(crate) async fn handle_tachi_arena(
    _server: &MemoryServer,
    params: TachiArenaParams,
) -> Result<String, String> {
    match params.action.to_ascii_lowercase().as_str() {
        "open" => handle_open(params),
        "spawn" => handle_spawn(_server, params).await,
        "board" => handle_board(params),
        "collect" => handle_collect(params),
        "abort" => handle_abort(params),
        "reap" => handle_reap(params),
        "close" => handle_close(params),
        other => Err(format!(
            "Invalid action '{other}'. Use open, spawn, board, collect, abort, reap, or close."
        )),
    }
}

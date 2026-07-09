mod context;
mod pack;
mod render;

use crate::tool_params::TachiProfileParams;
use crate::MemoryServer;
use serde_json::{json, Value};

use self::context::render_profile_context;
use self::pack::{pack_summary, resolve_pack};
use self::render::{render_profile_target, render_targets};

pub(crate) async fn handle_tachi_profile(
    server: &MemoryServer,
    params: TachiProfileParams,
) -> Result<String, String> {
    if !params.dry_run {
        return Err(
            "tachi_profile is read-only in this release; dry_run=false is not supported"
                .to_string(),
        );
    }

    match params.action.trim().to_ascii_lowercase().as_str() {
        "import" => {
            let pack = resolve_pack(&params)?;
            serialize_profile_response(json!({
                "status": "completed",
                "action": "import",
                "dry_run": true,
                "pack": pack,
            }))
        }
        "render" => {
            let pack = resolve_pack(&params)?;
            let targets = render_targets(&params);
            let rendered = targets
                .iter()
                .map(|target| render_profile_target(&pack, target, true))
                .collect::<Result<Vec<_>, String>>()?;
            serialize_profile_response(json!({
                "status": "completed",
                "action": "render",
                "dry_run": true,
                "pack_summary": pack_summary(&pack),
                "documents": rendered,
            }))
        }
        "context" => {
            let pack = resolve_pack(&params)?;
            let context = render_profile_context(server, &pack, &params);
            serialize_profile_response(json!({
                "status": "completed",
                "action": "context",
                "dry_run": true,
                "pack_summary": pack_summary(&pack),
                "context": context,
                "prepend_context": context,
            }))
        }
        other => Err(format!(
            "Unsupported tachi_profile action '{other}'. Expected import|render|context"
        )),
    }
}

fn serialize_profile_response(value: Value) -> Result<String, String> {
    serde_json::to_string_pretty(&value)
        .map_err(|e| format!("Failed to serialize tachi_profile response: {e}"))
}

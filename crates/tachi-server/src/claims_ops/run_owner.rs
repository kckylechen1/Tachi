//! Current principal, project and physical source shared by run access and input.
use crate::MemoryServer;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunOwner {
    pub(crate) agent_identity_id: String,
    pub(crate) canonical_project: String,
    pub(crate) source_fingerprint: String,
}

pub(crate) fn current_run_owner(server: &MemoryServer) -> Result<RunOwner, String> {
    let Some((Some(agent_identity_id), _, _)) = server.work_claim_connection() else {
        return Err("run owner unavailable".into());
    };
    if super::current_admission_receipt_ref(server)?.is_none()
        || !crate::memory_resources::sandbox_policy_clear(server)?
    {
        return Err("run admission unavailable".into());
    }
    let project = server.session_project().ok_or("run project unavailable")?;
    let (canonical_project, _) = server.resolve_server_named_project_binding(&project)?;
    let source_fingerprint =
        server.with_named_project_store_read_identity_checked(&canonical_project, |store| {
            crate::memory_resources::verified_project_source(server, &canonical_project, store)
                .ok_or_else(|| "project source unavailable".to_string())
        })?;
    Ok(RunOwner {
        agent_identity_id,
        canonical_project,
        source_fingerprint,
    })
}

use crate::server_state::MemoryServer;
use memcore::HubCapability;

impl MemoryServer {
    pub(crate) fn get_capability(&self, cap_id: &str) -> Result<HubCapability, rmcp::ErrorData> {
        let mut found = None;
        if self.has_project_db() {
            found = self
                .with_project_store_read(|store| {
                    store
                        .hub_get(cap_id)
                        .map_err(|e| format!("hub get project: {e}"))
                })
                .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        }
        if found.is_none() {
            found = self
                .with_global_store_read(|store| {
                    store
                        .hub_get(cap_id)
                        .map_err(|e| format!("hub get global: {e}"))
                })
                .map_err(|e| rmcp::ErrorData::internal_error(e, None))?;
        }
        found.ok_or_else(|| {
            rmcp::ErrorData::invalid_params(format!("Capability '{cap_id}' not found"), None)
        })
    }

    pub(crate) fn record_capability_call_outcome(
        &self,
        cap_id: &str,
        success: bool,
        error_kind: Option<&str>,
    ) -> Result<(), String> {
        const OPEN_THRESHOLD: u32 = 3;

        if self.has_project_db() {
            let in_project = self.with_project_store_read(|store| {
                store
                    .hub_get(cap_id)
                    .map(|cap| cap.is_some())
                    .map_err(|e| format!("hub get project: {e}"))
            })?;
            if in_project {
                return self.with_project_store(|store| {
                    store
                        .hub_record_call_outcome(cap_id, success, error_kind, OPEN_THRESHOLD)
                        .map_err(|e| format!("hub call outcome project: {e}"))
                });
            }
        }

        self.with_global_store(|store| {
            store
                .hub_record_call_outcome(cap_id, success, error_kind, OPEN_THRESHOLD)
                .map_err(|e| format!("hub call outcome global: {e}"))
        })
    }
}

use super::*;
use crate::manifest::DbRole;
use chrono::{DateTime, Utc};

fn entry(role: DbRole, scope_hint: &str) -> crate::manifest::DbEntry {
    crate::manifest::DbEntry {
        path: "/tmp/status/memory.db".to_string(),
        role,
        owner: "test".to_string(),
        schema_kind: "tachi".to_string(),
        vec_enabled: true,
        allow_write: true,
        last_doctor_at: String::new(),
        last_classification: "healthy".to_string(),
        scope_hint: scope_hint.to_string(),
        notes: String::new(),
    }
}

fn daemon_running() -> serde_json::Value {
    serde_json::json!({ "running": true })
}

mod coldpath_scoping;
mod daemon_manifest;
mod dispatch_eval;
mod markers_errors;
mod snapshot_labels;
mod unregistered_project_dbs;
mod vector_namespace;
mod warnings_score;

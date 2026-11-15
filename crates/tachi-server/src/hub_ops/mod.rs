mod call;
mod claude_export_fs;
mod discover;
mod export;
mod quick_add;
mod register;
mod review;
mod security_scan;

// Re-export all pub(crate) handler functions so main.rs can import them
pub(crate) use call::{handle_run_skill, handle_tachi_audit_log, read_registered_skill_document};
pub(crate) use discover::{
    handle_hub_discover, handle_hub_feedback, handle_hub_get, handle_hub_stats,
};
pub(crate) use export::handle_export_skills;
pub(crate) use quick_add::handle_hub_quick_add;
pub(crate) use register::handle_hub_register;
pub(crate) use review::{handle_hub_review, handle_hub_set_enabled};

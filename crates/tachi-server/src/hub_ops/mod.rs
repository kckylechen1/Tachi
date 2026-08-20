mod call;
mod discover;
mod export;
mod quick_add;
mod register;
mod review;
mod security_scan;
mod virtual_cap;

// Re-export all pub(crate) handler functions so main.rs can import them
pub(crate) use call::{
    execute_registered_skill_prompt, handle_hub_call, handle_hub_disconnect, handle_run_skill,
    handle_tachi_audit_log,
};
pub(crate) use discover::{
    handle_hub_discover, handle_hub_feedback, handle_hub_get, handle_hub_stats,
};
pub(crate) use export::handle_export_skills;
pub(crate) use quick_add::handle_hub_quick_add;
pub(crate) use register::handle_hub_register;
pub(crate) use review::{handle_hub_review, handle_hub_set_active_version, handle_hub_set_enabled};
pub(crate) use tachi_hub::{
    build_skill_execution_envelope, SkillExecutionMode, SIMULATED_SKILL_OUTPUT_MARKER,
    SIMULATED_SKILL_OUTPUT_WARNING,
};
pub(crate) use virtual_cap::{
    handle_vc_bind, handle_vc_list, handle_vc_register, handle_vc_resolve,
};

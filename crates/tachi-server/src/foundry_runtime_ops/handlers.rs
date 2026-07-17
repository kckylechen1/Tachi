mod bracket;
mod capture_session;
mod compact;
mod recall_context;
mod section;
mod target;

pub(crate) use capture_session::handle_capture_session;
pub(crate) use compact::{
    handle_compact_context, handle_compact_rollup, handle_compact_session_memory,
    COMPACT_CONTEXT_PERSIST_REFUSAL,
};
pub(crate) use recall_context::handle_recall_context;
pub(crate) use section::handle_section_build;

#[cfg(test)]
pub(super) use bracket::{
    build_bracket_self_evolution_id, classify_bracket_self_evolution,
    extract_bracket_self_evolution_notes, matches_agent_tag,
};
#[cfg(test)]
pub(super) use target::resolve_capture_target;

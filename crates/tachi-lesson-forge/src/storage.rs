//! The lesson-candidate memory domain tag.
//!
//! This module used to also hold the writer that persisted a forged
//! `LessonCandidateV1` as a pending `/lesson_candidates/<project>/<id>` row
//! (`persist_pending_lesson_candidate`) plus its private scrub/render/
//! metadata helpers. That writer never acquired a production caller — the
//! follow-up harness runner named in `mod.rs`'s "What this module explicitly
//! does NOT do" was never built — so it was deleted in #1564 along with the
//! other dormant contract leaves. The #1073 contract itself is untouched: a
//! real forge runner should write against the capture pipeline as it exists
//! when that runner is built, not restore this scaffolding.
//!
//! [`LESSON_CANDIDATE_DOMAIN`] stays because it is live regardless of who
//! writes: `memory_search_ops::search_memory::filters::is_lesson_candidate_entry`
//! reads it through the `tachi-server` shim as a containment-gate value on
//! every generic recall.

pub const LESSON_CANDIDATE_DOMAIN: &str = "lesson_candidate";

//! Read-only GitHub corpus source adapter (#1059).
//!
//! Frozen contract: `kckylechen1/tachi#1059`. Design authority:
//! `docs/engineering/architecture/issue-refinery-memory-lanes.md` §8 (+ §3
//! for `PullRequestSnapshotV1` fields).
//!
//! ## What this module is
//!
//! - [`pilot`] — freeze exactly the 20-case corpus pilot (spend gate:
//!   nothing adapts outside a frozen manifest).
//! - [`parse`] — pure gh issue/PR/event JSON → typed snapshots +
//!   [`CaseCorpusBundle`] (no network).
//! - [`adapt`] — pure `CaseCorpusBundle` → typed evidence refs + Pending
//!   `LessonCandidateV1` (never established).
//! - [`reader`] — read-only [`GithubCorpusReader`] trait + mutation-refusal
//!   surface; [`fetch_case_bundle`] uses only read methods.
//! - [`fixtures`] — synthetic gh JSON for RED→GREEN discrimination tests.
//!
//! ## What this module explicitly does NOT do
//!
//! - Write to GitHub (comment/label/close/reopen/body_edit/pr_mutate).
//! - Download or execute external links found in issue/PR bodies.
//! - Establish precedents (#950/#1077) or add GitHub lifecycle types to
//!   memcore (portable kernel stays free of GitHub SDK types).
//! - Invent an HTTP/octocrab client — callers supply already-fetched JSON
//!   via [`GithubCorpusReader`] or hand a pre-built [`CaseCorpusBundle`].

pub mod adapt;
pub mod fixtures;
pub mod live_pilot;
pub mod parse;
pub mod pilot;
pub mod reader;

#[cfg(test)]
mod tests;

pub use adapt::{
    adapt_corpus_case, AdaptError, CaseDraft, CorpusPilotReport, GithubCorpusCaseResult,
};
pub use parse::{
    assemble_case_bundle, parse_pr_snapshot_from_gh_json, CaseCorpusBundle, ParseError,
    ProvenanceEventKindV1, ProvenanceEventV1,
};
pub use pilot::{
    freeze_corpus_manifest, valid_20_cases, CorpusCaseV1, CorpusFreezeError, CorpusManifestV1,
    CORPUS_PILOT_SIZE,
};
pub use reader::{
    baseline_events_from_snapshots, fetch_case_bundle, refuse_github_mutation, FixtureCorpusReader,
    GithubCorpusReader, MutationProbe, FORBIDDEN_GITHUB_MUTATIONS,
};

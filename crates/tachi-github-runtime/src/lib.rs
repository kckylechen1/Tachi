//! GitHub-domain runtime carved out of `tachi-server` (#1610 Track T /
//! #1611 Track T3, carve 1).
//!
//! Selection criterion for this carve: nothing here names `MemoryServer` or
//! holds server state, so the tree lifts whole with no seam, no extension
//! trait, and no signature change. Anything added here that needs
//! `&MemoryServer` belongs in a later carve, after the runtime-core crate
//! exists.
//!
//! The gh-JSON → `IssueSnapshotV1` parser this adapter reuses deliberately
//! does NOT live here: it lives in `tachi_params::gh_json_parse`, beside the
//! `*V1` types it produces and the refinery hashing helpers it calls. This
//! crate consumes it like any other caller.

pub mod github_corpus_ops;

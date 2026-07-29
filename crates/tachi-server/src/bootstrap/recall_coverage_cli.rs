//! Explicit offline `tachi recall-coverage` route.
//!
//! This module intentionally has no daemon, manifest, migration, or default
//! path dependency. Its one database argument opens read-only and runs the
//! portable MemCore probe in-process.

use std::error::Error;
use std::path::Path;

use memcore::{
    format_recall_coverage_human, run_recall_coverage_probe, run_recall_coverage_probe_with_corpus,
    MemoryStore, RecallCoverageEquivalenceCorpus, RecallCoverageOptions,
};

pub(super) fn run_recall_coverage_command(
    db: &Path,
    top_k: Option<usize>,
    candidates_per_channel: Option<usize>,
    limit: Option<usize>,
    equivalence_file: Option<&Path>,
    human: bool,
) -> Result<(), Box<dyn Error>> {
    let db = db.to_str().ok_or_else(|| {
        format!(
            "recall coverage invariant: --db path is not valid UTF-8: {}",
            db.display()
        )
    })?;
    let store = MemoryStore::open_read_only(db)?;
    let mut options = RecallCoverageOptions {
        limit,
        ..Default::default()
    };
    if let Some(top_k) = top_k {
        options.top_k = top_k;
    }
    if let Some(candidates_per_channel) = candidates_per_channel {
        options.candidates_per_channel = candidates_per_channel;
    }
    let equivalence_corpus = equivalence_file
        .map(
            |path| -> Result<RecallCoverageEquivalenceCorpus, Box<dyn Error>> {
                let encoded = std::fs::read_to_string(path)?;
                let corpus: RecallCoverageEquivalenceCorpus = serde_json::from_str(&encoded)?;
                corpus.validate()?;
                Ok(corpus)
            },
        )
        .transpose()?;
    let report = if let Some(corpus) = equivalence_corpus.as_ref() {
        run_recall_coverage_probe_with_corpus(&store, options, corpus)?
    } else {
        run_recall_coverage_probe(&store, options)?
    };
    if human {
        print!("{}", format_recall_coverage_human(&report));
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

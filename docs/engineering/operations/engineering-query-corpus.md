# Engineering query regression corpus

`crates/memcore/src/search/tests/engineering_query_corpus.rs` preserves sanitized
query intentions from engineering conversations (#2176): why compact briefing
changed, a long Chinese follow-up, pending MCP skills work versus source review
and deployed runtime, current versus superseded rationale, and nonexistent
migration or acceptance records. All fixture records, IDs, dates and receipts are
synthetic. They describe no real PR, host, deployment or private database record.

The corpus calls the real in-memory SQLite `hybrid_search` pipeline (FTS and
symbolic channels). It uses normal candidate limits, fixed timestamps, no vectors,
no MMR and no access feedback. It does not call providers, the network or a live
store. Existing golden and operational corpus assertions remain independent.

Each answerable query declares expected record IDs and a maximum rank. The test
prints each expected ID's one-based rank, missing expected records, unrelated
results in the first five, stale results and actual result order. Noise means
anything outside that case's labeled expected set, including topically adjacent
references. Delivery-state labels must survive retrieval in record metadata;
reviewed and pending fixtures are not relabeled as deployed. The stale record is
explicitly superseded, with an opt-in historical retrieval as a positive control.

The absent identifier and absent opaque receipt ID queries require an empty result. A full
natural-language fabricated-premise query separately reports lexical neighbours
with `expected=[]`: those rows are noise, not evidence the imagined migration
happened. There is no semantic abstention or answer-generation layer in this
kernel test, so it does not assert the full question returns empty or validates
current real-world state. The full-question baseline fills all five result slots
with noise (`noise_at_5=5`, `stale=0`); this is a known retrieval gap, not a
successful answer about a migration or acceptance. The rank and missing-record
gates do not establish zero noise for natural-language questions.

Run the focused corpus with the repository's assigned Cargo build seat:

```text
cargo test -p memcore engineering_query_corpus -- --nocapture
```

Keep raw measurement output in the delivery artifacts. Add cases with explicit
ground truth before changing production ranking; do not weaken existing corpus
assertions to make a new case pass.

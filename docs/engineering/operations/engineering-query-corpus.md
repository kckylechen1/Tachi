# Engineering query regression corpus

`crates/memcore/src/search/tests/engineering_query_corpus.rs` preserves sanitized
query intentions from engineering conversations (#2176): why compact briefing
changed, a long Chinese follow-up, pending MCP skills work versus source review
and deployed runtime, current versus superseded rationale, and nonexistent
migration or acceptance records. All fixture records, IDs, dates and receipts are
synthetic. They describe no real PR, host, deployment or private database record.

The corpus calls the real in-memory SQLite `hybrid_search` pipeline (FTS and
symbolic channels). Its baseline uses normal candidate limits, fixed timestamps,
no vectors, no MMR and no access feedback. A separate synthetic-vector control
checks that the identifier rule is consistent across retrieval channels and the
attribution path. It does not call providers, the network or a live store.
Existing golden and operational corpus assertions remain independent.

Each answerable query declares expected record IDs and a maximum rank. The test
prints each expected ID's one-based rank, missing expected records, unrelated
results in the first five, stale results and actual result order. Noise means
anything outside that case's labeled expected set, including topically adjacent
references. Delivery-state labels must survive retrieval in record metadata;
reviewed and pending fixtures are not relabeled as deployed. The stale record is
explicitly superseded, with an opt-in historical retrieval as a positive control.

The absent identifier and absent opaque receipt ID queries require an empty
result. The full question about nonexistent `QuartzLedgerCutover` also requires
an empty result, even though its generic words produce candidates. Before the
identifier evidence gate that question filled all five slots with neighbours
(`noise_at_5=5`, `stale=0`). Those records did not identify the imagined migration.

For contextual questions, the gate recognizes explicit syntax in the original
query: `snake_case` symbols, `namespace::symbol`, UUIDs, camel-case compounds with
at least two internal lowercase-to-uppercase changes, and single symbols
explicitly enclosed in inline code. Bare, unquoted identifier queries retain
their existing exact-above-partial symbolic ranking. An opaque ticket such as
`YZQ-862915` does not make its presence mandatory in an otherwise answerable
question; use inline-code delimiters to make that literal constraint explicit.
The original symbolic probe-ranking and long-CJK-plus-ticket regressions remain
unchanged. Chinese questions containing a named code object are still gated.
Ordinary capitalized words, acronyms and two-part proper names such as `GitHub`,
`OpenAI` and `PowerShell` retain the existing evidence rules. Candidate fields must contain a
complete identifier, ignoring ASCII case; prefixes, suffixes and guessed
separator variants do not count. A standalone global `::` prefix is valid for
qualified names; other namespace prefixes and child suffixes do not count.
A declared name in `entities` or `keywords`
counts, so records written using an alias remain reachable when their stored
names identify the requested object. No semantic aliases are inferred.

For multiple identifiers, each ranked candidate must identify at least one
requested object. A comparison can therefore return separate records, including
evidence for an existing object when the other is absent. This is a bounded
retrieval condition, not semantic abstention, answer generation, proof of the
question's premise or verification of current real-world state. Questions without explicit
identifier syntax can still return topically adjacent records. The existing
rank and missing-record gates do not establish zero noise for all prose queries.
Optional graph expansion retains its existing, separately marked neighbour
results; with no eligible ranked seeds it has nothing to expand. This change
does not increase channel candidate limits or guarantee every stored named
record reaches the ranker.

Run the focused corpus with the repository's assigned Cargo build seat:

```text
cargo test -p memcore engineering_query_corpus -- --nocapture
```

Keep raw measurement output in the delivery artifacts. Add cases with explicit
ground truth before changing production ranking; do not weaken existing corpus
assertions to make a new case pass.

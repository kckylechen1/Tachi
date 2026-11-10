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

## Synthetic fixture visibility

Default recall excludes `/test`, `/tests`, `/scratch/tachi-test` and their
descendants, plus entries whose `metadata.synthetic_fixture` is the JSON boolean
`true`. The marker applies at any path, including `/eval`, `/wiki`, `/sft`, cards
and checkpoints. A string `"true"` or the number `1` is not this marker. Producers
can use either the reserved fixture path or the explicit metadata marker; no
existing data is relabeled, moved or deleted automatically.

`include_test=true` opts into these entries on memory search, ask and briefing.
Explicit path-scoped reads within the three reserved namespaces retain their
existing opt-in behavior. An unrelated path filter such as `/eval` does not opt
marked entries back in. `/testing`, `/test-results`, ordinary scratch notes and
real failure/avoidance lessons remain subject to their existing retrieval rules.
The option does not override archive, supersession, domain, sandbox or knowledge
lifecycle restrictions. Explicit `get(id)` remains available for inspection.

Automatic Staff and task contexts do not opt in. Their normal, SFT and avoidance
searches, retained knowledge reads, recent checkpoints, briefing Kanban rows and
seat-card projections apply the same default isolation. The engineering corpus
above deliberately sets `include_test=true` in its own search options so its
synthetic ground truth still exercises the retrieval-quality contract.

Sampled kernel search receipts report `excluded_test_rows`: unique candidates
rejected by this policy within that one store search's existing candidate and
graph budgets. This is not a whole-store inventory, a count of hypothetical
top-k results, or a claim about unexecuted searches or downstream sandbox ACLs.
The receipt contains no excluded IDs. Legacy JSON row arrays and Markdown
responses keep their existing shape; no default response envelope is added for
the diagnostic. Recall-cache keys include the option and invalidate earlier
cached packs when this policy changes.

The HTTP product scenario is available with
`python3 scripts/dogfood_e2e.py --tachi <candidate> --only synthetic`.

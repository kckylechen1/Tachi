# Lane-card source boundary

Sigil does not own editable lane-card declarations.

Owner ruling #1202 makes the governed Markdown dispatch-ledger the single
reviewed declaration source. `tachi cards sync` reads that source (or an
explicit `--dir`) into read-only `/cards/<seat>` mirror rows; prompt assembly
consumes the mirror and never writes declarations back.

The original `tachi-lane-card/v1` TOML seeds are preserved under
[`docs/archive/lane-cards-v1/`](../docs/archive/lane-cards-v1/) for evidence
archaeology only. They are not runtime configuration and must not be restored as
a competing card authority.

Typed dispatch profile/model configuration remains in `crates/tachi-dispatch`.
It may select a runtime model, but it does not replace model, harness, or seat
evidence cards.

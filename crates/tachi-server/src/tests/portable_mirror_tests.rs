//! Parity gate for `portable-server`'s hand-copied `tachi-params` search
//! literals.
//!
//! `crates/portable-server/src/service.rs` hand-mirrors three
//! `tachi-params::memory::search` values (its own comments call them a
//! "Portable mirror ... Keep in sync"): `MAX_SEARCH_TOP_K`,
//! `MAX_SEARCH_CANDIDATES_PER_CHANNEL`, and the `Some(0.85)` literal
//! returned by `default_mmr_threshold`. portable-server is deliberately
//! zero-dependency on `tachi-server`/`tachi-params` (see
//! `crates/portable-server/Cargo.toml`'s module doc: no operator/product
//! surfaces reachable by construction), so it cannot import the real
//! constants without breaking that isolation — this test lives here, in
//! `tachi-server` (which *does* depend on `tachi-params`), instead.
//!
//! Two of the three mirrored values are public constants: this test imports
//! the real `tachi_params::MAX_SEARCH_TOP_K` /
//! `tachi_params::MAX_SEARCH_CANDIDATES_PER_CHANNEL` and compares them
//! against literals extracted from portable-server's own source text. The
//! third, `default_mmr_threshold`, is a private serde-default fn inside
//! `tachi-params::memory::search` — not exported anywhere — so there is no
//! real value to import; its literal is extracted from both sides' source
//! text and compared symmetrically instead.
//!
//! Extraction is intentionally dumb: each helper anchors on the exact
//! const/fn name and expects a specific textual shape immediately after it.
//! It is not a parser — if either source's shape changes enough to break
//! the anchor, the test panics loudly (wrong-shape, not silently-wrong)
//! rather than comparing something it didn't actually find.

const PORTABLE_SERVICE_SRC: &str = include_str!("../../../portable-server/src/service.rs");
const TACHI_PARAMS_SEARCH_SRC: &str = include_str!("../../../tachi-params/src/memory/search.rs");

/// Find `"{name}: usize = <digits>;"` in `source` and parse `<digits>`.
/// Matches both `const NAME: usize = N;` and `pub const NAME: usize = N;`
/// (anchors on the part after any visibility keyword).
fn extract_usize_const(source: &str, name: &str) -> usize {
    let anchor = format!("{name}: usize = ");
    let start = source
        .find(&anchor)
        .unwrap_or_else(|| panic!("anchor `{anchor}` not found in source"))
        + anchor.len();
    let rest = &source[start..];
    let end = rest
        .find(';')
        .unwrap_or_else(|| panic!("no `;` terminator found after `{anchor}`"));
    rest[..end]
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse usize after `{anchor}`: {e}"))
}

/// Find `fn default_mmr_threshold() -> Option<f64> { Some(<literal>) }` in
/// `source` and parse `<literal>`.
fn extract_mmr_threshold(source: &str) -> f64 {
    const FN_ANCHOR: &str = "fn default_mmr_threshold() -> Option<f64> {";
    let after_fn = source
        .find(FN_ANCHOR)
        .unwrap_or_else(|| panic!("anchor `{FN_ANCHOR}` not found in source"))
        + FN_ANCHOR.len();
    let body = &source[after_fn..];
    const SOME_ANCHOR: &str = "Some(";
    let after_some = body
        .find(SOME_ANCHOR)
        .unwrap_or_else(|| panic!("no `{SOME_ANCHOR}` found after `{FN_ANCHOR}`"))
        + SOME_ANCHOR.len();
    let rest = &body[after_some..];
    let end = rest
        .find(')')
        .unwrap_or_else(|| panic!("no `)` terminator found after `{SOME_ANCHOR}`"));
    rest[..end]
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse f64 in default_mmr_threshold: {e}"))
}

#[test]
fn portable_max_search_top_k_mirrors_tachi_params() {
    let mirrored = extract_usize_const(PORTABLE_SERVICE_SRC, "MAX_SEARCH_TOP_K");
    assert_eq!(
        mirrored,
        tachi_params::MAX_SEARCH_TOP_K,
        "crates/portable-server/src/service.rs hand-mirrors \
         tachi_params::MAX_SEARCH_TOP_K ({}) as a plain constant ({mirrored}); \
         a change to tachi-params must be synced to the portable mirror too",
        tachi_params::MAX_SEARCH_TOP_K
    );
}

#[test]
fn portable_max_search_candidates_per_channel_mirrors_tachi_params() {
    let mirrored = extract_usize_const(PORTABLE_SERVICE_SRC, "MAX_SEARCH_CANDIDATES_PER_CHANNEL");
    assert_eq!(
        mirrored,
        tachi_params::MAX_SEARCH_CANDIDATES_PER_CHANNEL,
        "crates/portable-server/src/service.rs hand-mirrors \
         tachi_params::MAX_SEARCH_CANDIDATES_PER_CHANNEL ({}) as a plain \
         constant ({mirrored}); a change to tachi-params must be synced to \
         the portable mirror too",
        tachi_params::MAX_SEARCH_CANDIDATES_PER_CHANNEL
    );
}

#[test]
fn portable_default_mmr_threshold_mirrors_tachi_params() {
    // `tachi_params::memory::search::default_mmr_threshold` is a private
    // serde default fn, not a public constant — there is no real value to
    // import here (unlike the two `usize` consts above). Extract the
    // literal from both sides' own source text instead, symmetrically.
    let portable_value = extract_mmr_threshold(PORTABLE_SERVICE_SRC);
    let tachi_params_value = extract_mmr_threshold(TACHI_PARAMS_SEARCH_SRC);
    assert_eq!(
        portable_value, tachi_params_value,
        "crates/portable-server/src/service.rs::default_mmr_threshold \
         ({portable_value}) must stay in sync with \
         tachi-params::memory::search::default_mmr_threshold \
         ({tachi_params_value}) — a change to tachi-params must be synced \
         to the portable mirror too"
    );
}

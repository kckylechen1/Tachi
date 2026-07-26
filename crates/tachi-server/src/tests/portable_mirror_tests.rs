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

/// Find `fn default_mmr_threshold() -> Option<f64> { … Some(<literal>) … }` in
/// `source` and parse `<literal>`, searching only inside that function body
/// (brace-depth from the opening `{` of the fn — not the rest of the file).
fn extract_mmr_threshold(source: &str) -> f64 {
    const FN_ANCHOR: &str = "fn default_mmr_threshold() -> Option<f64> {";
    let fn_open = source
        .find(FN_ANCHOR)
        .unwrap_or_else(|| panic!("anchor `{FN_ANCHOR}` not found in source"))
        + FN_ANCHOR.len()
        - 1; // point at the opening `{`
    let body = function_body_slice(source, fn_open);
    // Search line-by-line with comments stripped so a decoy `Some(...)` in a
    // `//` comment cannot win over the real return.
    const SOME_ANCHOR: &str = "Some(";
    let mut after_some: Option<usize> = None;
    let mut cursor = 0usize;
    for line in body.split_inclusive('\n') {
        let code = code_before_line_comment(line);
        if let Some(rel) = code.find(SOME_ANCHOR) {
            after_some = Some(cursor + rel + SOME_ANCHOR.len());
            break;
        }
        cursor += line.len();
    }
    let after_some = after_some
        .unwrap_or_else(|| panic!("no `{SOME_ANCHOR}` found inside `{FN_ANCHOR}` function body"));
    let rest = &body[after_some..];
    let end = rest
        .find(')')
        .unwrap_or_else(|| panic!("no `)` terminator found after `{SOME_ANCHOR}`"));
    rest[..end]
        .trim()
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse f64 in default_mmr_threshold: {e}"))
}

/// Strip `//` line comments outside of double-quoted strings (same spirit as
/// `code_before_line_comment` in the docs censuses). Used so `{`/`}` in
/// comments cannot shift brace depth while scanning a function body.
fn code_before_line_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    let bytes = line.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let character = bytes[index];
        if escaped {
            escaped = false;
        } else if character == b'\\' && quoted {
            escaped = true;
        } else if character == b'"' {
            quoted = !quoted;
        } else if !quoted
            && character == b'/'
            && bytes.get(index + 1).is_some_and(|next| *next == b'/')
        {
            return &line[..index];
        }
        index += 1;
    }
    line
}

/// `open_brace_index` must point at the function's opening `{`. Returns the
/// slice from just after that `{` through the matching closing `}`.
/// Braces inside `//` line comments (outside quotes) are ignored.
fn function_body_slice(source: &str, open_brace_index: usize) -> &str {
    let bytes = source.as_bytes();
    assert_eq!(
        bytes.get(open_brace_index).copied(),
        Some(b'{'),
        "function_body_slice expects open_brace_index on '{{'"
    );
    let mut depth = 0_i64;
    let mut i = open_brace_index;
    while i < bytes.len() {
        // Process one logical line at a time so `//` comments can be stripped
        // before brace counting (comments may contain decoy `{` / `}`).
        let line_end = source[i..]
            .find('\n')
            .map(|offset| i + offset)
            .unwrap_or(bytes.len());
        let line = &source[i..line_end];
        // Strip `//` outside quotes before counting braces on this line.
        let code = code_before_line_comment(line);
        let code_bytes = code.as_bytes();
        let mut j = 0;
        let mut in_string = false;
        let mut escaped = false;
        while j < code_bytes.len() {
            let absolute = i + j;
            let b = code_bytes[j];
            if in_string {
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b'"' {
                    in_string = false;
                }
                j += 1;
                continue;
            }
            match b {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &source[open_brace_index + 1..absolute];
                    }
                }
                _ => {}
            }
            j += 1;
        }
        i = if line_end < bytes.len() {
            line_end + 1
        } else {
            bytes.len()
        };
    }
    panic!("function body braces did not close (unbalanced '{{' / '}}')");
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

#[test]
fn extract_mmr_threshold_ignores_braces_inside_line_comments() {
    let source = r#"
fn default_mmr_threshold() -> Option<f64> {
    // decoy { Some(0.99)
    Some(0.85)
}
fn other() { Some(0.99) }
"#;
    assert_eq!(
        extract_mmr_threshold(source),
        0.85,
        "comment decoy Some(0.99) / '{{' must not widen the body past the real fn"
    );
}

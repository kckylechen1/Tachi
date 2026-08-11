//! Census validation for `TachiTaskParams` field dispositions (#1319 C1).
//!
//! This is a documentation-only contract test. It loads
//! `docs/engineering/architecture/task-field-disposition-census-v1.fixture.json`
//! and asserts that the machine-verifiable map still matches the live struct:
//!
//! 1. `total_fields` equals the count of `pub <name>:` fields actually
//!    declared on `TachiTaskParams` in `crates/tachi-params/src/facade/task.rs`.
//! 2. Every fixture field name exists on the live struct (no phantoms).
//! 3. Every live struct field has a fixture entry (no census gaps).
//! 4. `disposition_summary` counts sum to `total_fields` and match the actual
//!    per-disposition grouping of fixture entries.
//!
//! If any of those drift, the test fails and forces a census refresh before
//! [1319-C2] can freeze the deletion set. There is NO runtime behavior change
//! here — this module only reads source text and a JSON fixture.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const FIXTURE: &str = include_str!(
    "../../../../../docs/engineering/architecture/task-field-disposition-census-v1.fixture.json"
);

const TASK_PARAMS_STRUCT_PATH: &str = "crates/tachi-params/src/facade/task.rs";

const EXPECTED_SCHEMA_VERSION: &str = "task_field_disposition.v1";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tachi-server lives under <repo>/crates")
        .to_path_buf()
}

/// Strip a trailing `// ...` line comment, honoring `"` / `\` quoting so a
/// `//` inside a string literal is not mistaken for a comment.
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

/// Scan the `TachiTaskParams` struct source and collect, in declaration
/// order, the names of every `pub <name>:` field. This mirrors the
/// source-scanning approach used by the external-staffing-contract test so
/// the census fails loudly if a field is added/removed without refreshing
/// the fixture.
fn collect_tachi_task_param_fields(source: &str) -> Vec<String> {
    // Walk lines, track brace depth so we only collect fields that live inside
    // the `pub struct TachiTaskParams {` body (and skip nested #[cfg(test)]
    // helper structs / `mod tests`). Field declarations look like:
    //   pub <name>: <Type>,
    // possibly preceded by `#[serde(...)]` / `#[schemars(...)]` attributes
    // and spanning attribute lines. We only match lines whose first non-space
    // token after stripping attributes is `pub`.
    let mut fields = Vec::new();
    let mut in_struct = false;
    let mut struct_depth: i64 = 0;
    let mut struct_seen_open = false;

    for raw_line in source.lines() {
        let line = code_before_line_comment(raw_line);
        let trimmed = line.trim();

        // Detect struct header (single line). `pub struct TachiTaskParams {`
        if !in_struct
            && trimmed.contains("struct TachiTaskParams")
            && trimmed.contains("pub struct")
        {
            in_struct = true;
            struct_seen_open = trimmed.contains('{');
            struct_depth = if struct_seen_open { 1 } else { 0 };
            continue;
        }

        if in_struct {
            // Update brace depth based on this line (only counting braces that
            // are not inside string literals — code_before_line_comment already
            // stripped comments, but strings may still contain braces).
            for (chr, quoted_state) in iterate_non_string_chars(line) {
                let _ = chr;
                let _ = quoted_state;
            }
            // Simpler + sufficient: count raw braces on the line. Field types
            // here never embed unbalanced braces in string literals because
            // types like `serde_json::Value` and `Vec<String>` are brace-free,
            // and any `#[serde(...)]` attribute is itself parenthesized, not
            // braced. This stays correct for the current struct.
            let open = line.matches('{').count();
            let close = line.matches('}').count();

            if !struct_seen_open {
                if open > 0 {
                    struct_seen_open = true;
                    struct_depth = open as i64 - close as i64;
                }
                continue;
            }

            struct_depth += open as i64 - close as i64;

            // Once struct_depth drops to 0 (or below), the struct body ended.
            if struct_depth <= 0 {
                in_struct = false;
                struct_seen_open = false;
                continue;
            }

            // Field declaration detection: a line that starts with `pub ` and
            // contains a `:` BEFORE any `=` (so we don't match consts/macros).
            // We also reject lines that declare nested items (fn, struct,
            // mod, trait, impl) just in case.
            if let Some(name) = struct_field_name(trimmed) {
                fields.push(name);
            }
        }
    }

    fields
}

/// Minimal char iterator stub kept for clarity; we rely on raw brace counting
/// instead. Returns an empty iterator — present only so the call site
/// documents the consideration.
fn iterate_non_string_chars(_line: &str) -> impl Iterator<Item = (char, bool)> {
    core::iter::empty()
}

/// If `trimmed` looks like `pub <name>: <Type>,` return `<name>`.
fn struct_field_name(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with("pub ") {
        return None;
    }
    // Skip attribute lines and vis-only lines.
    if trimmed.starts_with("#[") {
        return None;
    }
    if trimmed.contains(" fn ")
        || trimmed.contains(" struct ")
        || trimmed.contains(" mod ")
        || trimmed.contains(" trait ")
        || trimmed.contains(" impl ")
        || trimmed.contains(" enum ")
        || trimmed.contains(" const ")
        || trimmed.contains(" type ")
    {
        return None;
    }
    let after_pub = &trimmed["pub ".len()..];
    // Stop at first `:` (the type separator) — names are simple identifiers.
    let colon = after_pub.find(':')?;
    // Reject any `=` before the colon (e.g. `pub const X: ... = ...`).
    let eq = after_pub.find('=');
    if let Some(eq_index) = eq {
        if eq_index < colon {
            return None;
        }
    }
    let name = after_pub[..colon].trim();
    // Field names are ascii_snake_case identifiers.
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    if name.is_empty() || name.chars().next().unwrap().is_ascii_digit() {
        return None;
    }
    Some(name.to_string())
}

fn load_fixture() -> Value {
    serde_json::from_str(FIXTURE)
        .expect("task-field-disposition-census-v1.fixture.json must be valid JSON")
}

#[test]
fn task_field_disposition_census_matches_live_struct() {
    let root = repo_root();
    let struct_source_path = root.join(TASK_PARAMS_STRUCT_PATH);
    let struct_source = std::fs::read_to_string(&struct_source_path)
        .unwrap_or_else(|err| panic!("read {TASK_PARAMS_STRUCT_PATH}: {err}"));

    let live_fields = collect_tachi_task_param_fields(&struct_source);
    assert!(
        !live_fields.is_empty(),
        "scanner found zero fields on TachiTaskParams — parser likely broken"
    );

    let fixture = load_fixture();

    // (1) schema_version is the expected contract tag.
    let schema_version = fixture
        .get("schema_version")
        .and_then(Value::as_str)
        .expect("fixture has schema_version");
    assert_eq!(
        schema_version, EXPECTED_SCHEMA_VERSION,
        "fixture schema_version drifted; regenerate the census"
    );

    // (2) total_fields equals the live struct field count.
    let claimed_total = fixture
        .get("total_fields")
        .and_then(Value::as_u64)
        .expect("fixture has total_fields") as usize;
    assert_eq!(
        claimed_total,
        live_fields.len(),
        "fixture total_fields ({}) != live TachiTaskParams field count ({}) — \
         refresh the census (add/remove the field entry and adjust disposition_summary)",
        claimed_total,
        live_fields.len()
    );

    // (3) Every fixture field exists on the live struct (no phantoms).
    let live_set: BTreeSet<&str> = live_fields.iter().map(String::as_str).collect();
    let fixture_fields_obj = fixture
        .get("fields")
        .and_then(Value::as_object)
        .expect("fixture has fields object");
    let fixture_field_count = fixture_fields_obj.len();
    assert_eq!(
        fixture_field_count,
        live_fields.len(),
        "fixture fields map has {} entries but live struct has {} fields — \
         census must cover every field exactly once",
        fixture_field_count,
        live_fields.len()
    );

    let mut phantom: Vec<&str> = Vec::new();
    for name in fixture_fields_obj.keys() {
        if !live_set.contains(name.as_str()) {
            phantom.push(name);
        }
    }
    assert!(
        phantom.is_empty(),
        "fixture references fields not present on TachiTaskParams: {phantom:?} \
         (struct has {live_fields:?})"
    );

    // (4) Every live field has a fixture entry (no census gaps).
    let fixture_set: BTreeSet<&str> = fixture_fields_obj.keys().map(String::as_str).collect();
    let missing: Vec<&str> = live_fields
        .iter()
        .map(String::as_str)
        .filter(|name| !fixture_set.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "TachiTaskParams fields missing from census fixture: {missing:?}"
    );

    // (5) Every fixture entry has a well-formed body and a valid disposition.
    let allowed_dispositions: BTreeSet<&str> = [
        "delete",
        "move_to_staff",
        "operator_only_internal",
        "keep_task_shared",
        "keep_task_ledger",
        "defer_1467",
    ]
    .iter()
    .copied()
    .collect();
    let allowed_owners: BTreeSet<&str> = ["Task", "Staff", "CLI/internal", "deleted"]
        .iter()
        .copied()
        .collect();
    for (name, entry) in fixture_fields_obj {
        let disposition = entry
            .get("disposition")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("field {name} missing disposition"));
        assert!(
            allowed_dispositions.contains(disposition),
            "field {name} has unknown disposition {disposition:?}"
        );
        let owner = entry
            .get("canonical_future_owner")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("field {name} missing canonical_future_owner"));
        assert!(
            allowed_owners.contains(owner),
            "field {name} has unknown canonical_future_owner {owner:?}"
        );
        let actions = entry
            .get("used_by_actions")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("field {name} missing used_by_actions"));
        for action in actions {
            assert!(
                action.is_string(),
                "field {name} has non-string action entry: {action:?}"
            );
        }
        let rationale = entry
            .get("rationale")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("field {name} missing rationale"));
        assert!(
            !rationale.trim().is_empty(),
            "field {name} has empty rationale"
        );
    }

    // (6) disposition_summary sums to total_fields AND matches the actual
    // per-disposition grouping of fixture entries.
    let summary = fixture
        .get("disposition_summary")
        .and_then(Value::as_object)
        .expect("fixture has disposition_summary");
    let summary_sum: u64 = summary.values().filter_map(Value::as_u64).sum();
    assert_eq!(
        summary_sum, claimed_total as u64,
        "disposition_summary counts sum to {summary_sum} but total_fields is {claimed_total}"
    );

    let mut actual_by_disposition: BTreeMap<&str, u64> = BTreeMap::new();
    for entry in fixture_fields_obj.values() {
        let d = entry
            .get("disposition")
            .and_then(Value::as_str)
            .expect("entry has disposition");
        *actual_by_disposition.entry(d).or_insert(0) += 1;
    }
    for (disposition, actual_count) in &actual_by_disposition {
        let claimed = summary
            .get(*disposition)
            .and_then(Value::as_u64)
            .unwrap_or(0);
        assert_eq!(
            *actual_count, claimed,
            "disposition {disposition}: fixture has {actual_count} entries but summary claims {claimed}"
        );
    }
    // Every summary key must correspond to at least zero actual entries; if a
    // summary key names a disposition that has no entries at all, that is fine
    // only if its claimed count is also zero.
    for (disposition, claimed) in summary.iter() {
        let actual = actual_by_disposition
            .get(disposition.as_str())
            .copied()
            .unwrap_or(0);
        assert_eq!(
            actual, *claimed,
            "disposition {disposition}: summary claims {claimed} but fixture has {actual} entries"
        );
    }
}

/// #1319-C2 discriminator: every action named in any field's `used_by_actions`
/// must be a LIVE action in the current `TachiTaskAction` inventory. After
/// [1319-C2] removed Dispatch/Wait/Cancel, the census must not keep naming
/// them — a field whose only readers were removed must be re-dispositioned.
/// This is the semantic check the field-count checks cannot catch.
#[test]
fn census_used_by_actions_are_live_primary_actions() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("census fixture parses");
    let live: BTreeSet<&str> = tachi_params::TachiTaskAction::PRIMARY
        .iter()
        .map(|action| action.as_str())
        .collect();
    let removed: BTreeSet<&str> = [
        "dispatch",
        "wait",
        "cancel",
        "plan",
        "cycle_plan",
        "recommend",
        "refine_issues",
        "merge",
        "ux_matrix",
        "briefing",
        "doc_index",
        "cycle_status",
    ]
    .into_iter()
    .collect();

    let fields = fixture["fields"].as_object().expect("census fields");
    let mut stale = Vec::new();
    for (field, entry) in fields {
        let Some(actions) = entry["used_by_actions"].as_array() else {
            continue;
        };
        for action in actions {
            let name = action.as_str().unwrap_or_default();
            if !live.contains(name) {
                stale.push(format!(
                    "{field}: used_by_actions contains '{name}' (not a live PRIMARY action)"
                ));
            }
            if removed.contains(name) {
                stale.push(format!(
                    "{field}: used_by_actions contains REMOVED action '{name}' (#1319-C2/#1683-C1a/#1712-C1b)"
                ));
            }
        }
    }
    assert!(
        stale.is_empty(),
        "census used_by_actions references stale/removed actions:\n{}",
        stale.join("\n")
    );
}

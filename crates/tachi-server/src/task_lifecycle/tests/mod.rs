use super::*;

#[test]
fn parse_issue_ref_owner_repo_form() {
    let t = parse_issue_ref("owner/repo#123", None).expect("owner/repo#N");
    assert_eq!(t.repo, "owner/repo");
    assert_eq!(t.number, 123);
    // trailing slash + surrounding whitespace are trimmed
    let t = parse_issue_ref("  owner/repo#5/ ", None).expect("trimmed");
    assert_eq!((t.repo.as_str(), t.number), ("owner/repo", 5));
}

#[test]
fn parse_issue_ref_url_and_hash_forms() {
    let t = parse_issue_ref("https://github.com/o/r/issues/7", None).expect("issue url");
    assert_eq!((t.repo.as_str(), t.number), ("o/r", 7));
    // bare #N needs a valid default repo
    let t = parse_issue_ref("#9", Some("o/r")).expect("#N with default");
    assert_eq!((t.repo.as_str(), t.number), ("o/r", 9));
}

#[test]
fn parse_issue_ref_rejects_bad_input() {
    assert!(parse_issue_ref("", None).is_none());
    assert!(parse_issue_ref("#9", None).is_none(), "no default repo");
    assert!(parse_issue_ref("#9", Some("noslash")).is_none(), "bad repo");
    assert!(
        parse_issue_ref("owner/repo#abc", None).is_none(),
        "non-numeric"
    );
    assert!(parse_issue_ref("a/b/c#1", None).is_none(), "two slashes");
    // a PR URL is not an issue ref
    assert!(parse_issue_ref("https://github.com/o/r/pull/7", None).is_none());
}

#[test]
fn parse_pr_ref_forms_and_rejections() {
    let t = parse_pr_ref("owner/repo#42").expect("owner/repo#N");
    assert_eq!((t.repo.as_str(), t.number), ("owner/repo", 42));
    let t = parse_pr_ref("https://github.com/o/r/pull/8").expect("pull url");
    assert_eq!((t.repo.as_str(), t.number), ("o/r", 8));
    // an issue URL is not a PR ref; bare #N has no repo
    assert!(parse_pr_ref("https://github.com/o/r/issues/8").is_none());
    assert!(parse_pr_ref("#8").is_none());
    assert!(parse_pr_ref("").is_none());
}

#[test]
fn slug_for_branch_normalizes_and_floors() {
    assert_eq!(slug_for_branch("Fix the Bug"), "fix-the-bug");
    assert_eq!(slug_for_branch("a   b"), "a-b"); // runs collapse to one dash
    assert_eq!(slug_for_branch("feature/foo:bar"), "feature-foo-bar");
    assert_eq!(slug_for_branch("  Lead"), "lead"); // no leading dash
    assert_eq!(slug_for_branch(""), "work"); // empty floor
    assert_eq!(slug_for_branch("!!!"), "work"); // no alphanumerics
    let long = slug_for_branch(&"x".repeat(60));
    assert!(long.len() <= 48 && !long.ends_with('-'));
}

#[test]
fn normalize_issue_ref_roundtrips_or_errors() {
    assert_eq!(
        normalize_issue_ref("https://github.com/o/r/issues/3").unwrap(),
        "o/r#3"
    );
    assert_eq!(normalize_issue_ref("o/r#3").unwrap(), "o/r#3");
    assert!(normalize_issue_ref("#3").is_err(), "no default repo");
    assert!(normalize_issue_ref("garbage").is_err());
}

#[test]
fn initial_merge_state_maps_pr_state() {
    assert_eq!(initial_merge_state_for_pr(Some("MERGED")), "merged");
    assert_eq!(initial_merge_state_for_pr(Some("merged")), "merged"); // case-insensitive
    assert_eq!(initial_merge_state_for_pr(Some("CLOSED")), "blocked");
    assert_eq!(initial_merge_state_for_pr(Some("OPEN")), "pending");
    assert_eq!(initial_merge_state_for_pr(None), "pending");
}

#[test]
fn string_array_field_trims_and_guards() {
    let v = json!({ "items": ["x", " y ", "", "  ", "z"] });
    assert_eq!(string_array_field(&v, "items"), vec!["x", "y", "z"]);
    assert!(string_array_field(&v, "missing").is_empty());
    assert!(string_array_field(&json!({ "items": "notarray" }), "items").is_empty());
}

#[test]
fn extract_markdown_paths_only_doc_md() {
    let got = extract_markdown_paths("edit `docs/a.md`, also lib.md, and src/docs/b.md done");
    assert_eq!(got, vec!["docs/a.md", "src/docs/b.md"]);
    // a bare README.md (no docs/ prefix, no /docs/ segment) is excluded
    assert!(extract_markdown_paths("README.md changelog.md").is_empty());
}

/// kckylechen1/tachi#1058: an outer `intake` call's `format="full"` used to
/// get silently discarded once `intake_briefing_params` overwrote the inner
/// briefing call's `format` to `"json"` — the inner `compact` stayed `None`
/// and defaulted to the tight 4-row packet regardless of what the caller
/// asked for. `intake_briefing_params` must read the *original* `format`
/// intent before the overwrite and reverse-pressure `compact` explicitly.
#[test]
fn intake_briefing_params_reverse_pressures_compact_for_format_full() {
    let issue = IssueSnapshot {
        repo: "o/r".to_string(),
        number: 1,
        title: "t".to_string(),
        body: None,
        labels: Vec::new(),
        state: Some("open".to_string()),
        url: "https://github.com/o/r/issues/1".to_string(),
        doc_paths: Vec::new(),
        spec_paths: Vec::new(),
    };

    // Outer intake call requested the full board but never touched `compact`.
    let outer_full: TachiTaskParams = serde_json::from_value(json!({
        "action": "intake",
        "format": "full",
    }))
    .expect("deserialize intake params with format=full");
    let briefing_full = intake_briefing_params(&outer_full, "flow_x", "objective", &issue);
    assert_eq!(
        briefing_full.compact,
        Some(false),
        "format=full intake must reverse-pressure the inner briefing call to \
         compact=false, not leave it None to fall back to the compact default"
    );
    // format is still forced to json for the machine-readable inner call.
    assert_eq!(briefing_full.format.as_deref(), Some("json"));

    // An explicit compact still wins outright over format=full.
    let outer_explicit: TachiTaskParams = serde_json::from_value(json!({
        "action": "intake",
        "format": "full",
        "compact": true,
    }))
    .expect("deserialize intake params with explicit compact");
    let briefing_explicit = intake_briefing_params(&outer_explicit, "flow_x", "objective", &issue);
    assert_eq!(
        briefing_explicit.compact,
        Some(true),
        "an explicit compact=true must not be overridden by format=full"
    );

    // format=json (the default) must not force compact=false.
    let outer_json: TachiTaskParams = serde_json::from_value(json!({
        "action": "intake",
        "format": "json",
    }))
    .expect("deserialize intake params with format=json");
    let briefing_json = intake_briefing_params(&outer_json, "flow_x", "objective", &issue);
    assert_eq!(
        briefing_json.compact, None,
        "format=json must leave compact untouched (defaults to compact packet downstream)"
    );
}

#[test]
fn dedupe_strings_keeps_first_occurrence() {
    let mut v = vec![
        "a".to_string(),
        "b".to_string(),
        "a".to_string(),
        "c".to_string(),
        "b".to_string(),
    ];
    dedupe_strings(&mut v);
    assert_eq!(v, vec!["a", "b", "c"]);
}

// ─── Shared run-root test fixture ───────────────────────────────────────────
//
// GitHub flow-state tests (and future flow-artifact tests) need an isolated
// `TACHI_RUN_ROOT` so they never collide with a developer's real `.tachi/runs`
// or with each other. `global_test_lock` is the shared cfg-test mutex
// that already serializes every run-root test across the crate; we reuse it
// (rather than inventing a second lock) so all run-root tests stay mutually
// exclusive. This mirrors the `shell_ops/tests` harness convention: the mutex
// guard is held for the test's lifetime and each test resolves a unique fixture
// path from the current timestamp, so concurrent tests cannot trample each
// other's run directory even though `TACHI_RUN_ROOT` is not restored on drop.

fn runs_env_lock() -> &'static std::sync::Mutex<()> {
    crate::utils::global_test_lock()
}

struct RunsRootGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
    path: PathBuf,
}

impl std::ops::Deref for RunsRootGuard {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.path
    }
}

fn temp_runs_root() -> RunsRootGuard {
    let guard = runs_env_lock().lock().unwrap_or_else(|e| e.into_inner());
    let d = crate::utils::test_fixture_path(format!(
        "tachi-task-lifecycle-test-{}",
        Utc::now().format("%Y%m%dT%H%M%S%fZ")
    ));
    std::fs::create_dir_all(&d).unwrap();
    // SAFETY: `set_var` is unsafe on edition 2021 because it can race with
    // other threads reading the same env key. This call is safe because:
    //   1. The shared `global_test_lock` mutex is held for the entire
    //      lifetime of `RunsRootGuard`, serialising all `temp_runs_root()`
    //      callers across the crate.
    //   2. Each test resolves a unique fixture path from the current
    //      timestamp, so even though `TACHI_RUN_ROOT` is not restored on
    //      drop, concurrent tests cannot trample each other's run directory.
    //   3. No production code path mutates `TACHI_RUN_ROOT`.
    unsafe {
        std::env::set_var("TACHI_RUN_ROOT", &d);
    }
    RunsRootGuard {
        _guard: guard,
        path: d,
    }
}

mod github_status_events;

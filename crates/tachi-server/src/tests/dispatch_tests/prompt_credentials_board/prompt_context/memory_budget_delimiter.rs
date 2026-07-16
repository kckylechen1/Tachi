use super::*;

/// Discriminating regression for the memory-budget/untrusted-boundary
/// ordering bug flagged in cross-vendor review of #1149 (Refs #1036): the
/// aggregate character budget must be applied to the RAW recalled body
/// before it is wrapped in the `<untrusted_content>` boundary, never after.
/// Truncating the already-wrapped string can sever the closing boundary tag
/// -- untrusted content still cannot escape (nothing after the cut becomes
/// executable), but the structural authority of the boundary for every
/// section rendered afterward is silently broken (an unclosed tag "eats"
/// the rest of the prompt). This test fails on the pre-fix ordering because
/// the closing tag for the over-budget entry never appears in the prompt at
/// all.
// Plain `#[test]` + `block_on` (not `#[tokio::test]`), matching the
// `global_test_lock` convention used everywhere else in this crate (e.g.
// `bootstrap::serve::stdio::tests`): the guard protects the process-wide
// `TACHI_DISPATCH_MEMORY_CONTEXT_BUDGET_CHARS` var against a parallel test
// racing the same env key, so it must stay held for the entire
// `assemble_prompt` call including its internal awaits -- `block_on` runs
// that future to completion synchronously on this thread, so there is no
// `.await` expression in scope for clippy's `await_holding_lock` lint to
// flag, while the guard's actual coverage is unchanged.
#[test]
fn over_budget_memory_context_keeps_paired_untrusted_boundary() {
    // TACHI_DISPATCH_MEMORY_CONTEXT_BUDGET_CHARS is a process-global env var
    // read by every concurrent `assemble_prompt` call in this test binary;
    // guard it the same way the skill-budget regression test in
    // dispatch_ops::prompt::tests does.
    let _guard = crate::utils::global_test_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _budget = EnvVarGuard::set_value("TACHI_DISPATCH_MEMORY_CONTEXT_BUDGET_CHARS", "80");

    let server = make_server();
    let task = "zzqx1149 delimiter budget boundary regression probe";
    let path = "/scratch/sigil/zzqx1149-delimiter-budget";
    // Comfortably larger than the 80-char budget so the item is guaranteed
    // to be truncated (not fully admitted, not fully omitted -- top_k=5
    // against a single seeded row always surfaces it with budget to spend).
    let filler = "PADDING-CONTENT-".repeat(20);
    let raw_text = format!("{task} {filler}");

    server
        .with_global_store(|store| {
            let mut entry = make_entry("zzqx1149-delimiter-budget-entry");
            entry.path = path.to_string();
            entry.text = raw_text.clone();
            entry.summary = "delimiter budget boundary regression fixture".to_string();
            store.upsert(&entry).map_err(|e| e.to_string())
        })
        .expect("seed over-budget memory entry");

    let prompt = tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(crate::dispatch_ops::assemble_prompt(
            &server,
            &dispatch_params(Some("codex"), task),
        ));

    // Each rendered entry is one element of the prompt's `parts` vector,
    // joined with "\n\n"; our filler has no blank lines in it, so the first
    // "\n\n" after the header marks the true end of THIS entry's section
    // (isolating it from unrelated `<untrusted_content>` wraps elsewhere in
    // the prompt, e.g. the `## Task` section).
    let header = format!("### {path}\n");
    let header_at = prompt
        .find(&header)
        .unwrap_or_else(|| panic!("expected the seeded entry to render at all: {prompt}"));
    let rest = &prompt[header_at..];
    let section_end = rest.find("\n\n").unwrap_or(rest.len());
    let section = &rest[..section_end];

    let expected_prefix = format!("### {path}\n<untrusted_content>\n");
    assert!(
        section.starts_with(&expected_prefix),
        "the opening boundary tag must render intact right after the header: {section}"
    );
    assert_eq!(
        section.matches("<untrusted_content>").count(),
        1,
        "exactly one opening boundary tag for this entry: {section}"
    );
    assert_eq!(
        section.matches("</untrusted_content>").count(),
        1,
        "exactly one closing boundary tag for this entry: {section}"
    );
    assert!(
        section.ends_with("</untrusted_content>"),
        "closing boundary tag must survive the budget cut intact and be the last thing in \
         this entry's section -- a severed close tag leaves everything rendered after it \
         structurally inside an unclosed untrusted block: {section}"
    );

    let body = &section[expected_prefix.len()..section.len() - "</untrusted_content>".len()];
    let body = body.strip_suffix('\n').unwrap_or(body);
    assert!(
        body.contains("..."),
        "truncation must be explicitly marked in the admitted body, not a silent cut: {body}"
    );
    assert!(
        body.chars().count() < filler.chars().count(),
        "the raw filler body must actually have been shortened by the budget, not just \
         wrapped: {body}"
    );
    assert!(
        !prompt.contains(&filler),
        "the full oversized body must never reach the prompt verbatim: {prompt}"
    );

    // The truncation must be attributable: the aggregate budget summary
    // discloses which channel and how many items it clipped.
    assert!(
        prompt.contains("## Prompt input budget"),
        "the budget decision must be surfaced in the prompt: {prompt}"
    );
    assert!(
        prompt.contains(
            "memory input budget: admitted 80/80 characters; truncated 1 item(s), \
             omitted 0 body item(s)"
        ),
        "the memory budget summary must attribute the truncation to this item: {prompt}"
    );
}

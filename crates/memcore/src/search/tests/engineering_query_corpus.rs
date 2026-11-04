//! Engineering recall corpus (#2176). Query intentions are sanitized from
//! engineering conversations; all records, IDs, dates and receipts are invented.
//! No private database, provider or network is used. See
//! `docs/engineering/operations/engineering-query-corpus.md` for ground truth.

use super::*;

const CURRENT: &str = "eq-brief-rationale-current";
const STALE: &str = "eq-brief-rationale-superseded";
const PENDING: &str = "eq-mcp-skills-pending";
const REVIEWED: &str = "eq-mcp-skills-reviewed";
const DEPLOYED: &str = "eq-mcp-runtime-deployed";
const SOURCE: &str = "external:synthetic-engineering-query-corpus";

struct Seed {
    id: &'static str,
    text: &'static str,
    keywords: &'static [&'static str],
    evidence_state: &'static str,
}

const SEEDS: &[Seed] = &[
    Seed {
        id: CURRENT,
        text: "Tachi compact briefing changed because the earlier summary hid why decisions were made. The current rationale keeps the reason and evidence source next to each decision so a resumed session can recover the engineering context. Tachi 精简简报调整原因：旧摘要遗漏决策理由；现在保留为什么改、证据来源，恢复会话时不必重新猜测工程背景。",
        keywords: &["tachi", "compact", "briefing", "rationale", "精简简报", "决策理由"],
        evidence_state: "current_decision",
    },
    Seed {
        id: STALE,
        text: "Tachi compact briefing earlier rationale: reduce output length by keeping only the final decision. Discard the reason and evidence source from the summary. 旧版精简简报只留结论，省略为什么改和证据来源。",
        keywords: &["tachi", "compact", "briefing", "rationale", "精简简报"],
        evidence_state: "superseded_decision",
    },
    Seed {
        id: "eq-brief-usage",
        text: "Tachi compact briefing usage guide: request a short summary before resuming work. This reference describes the compact argument and response size, without explaining the engineering change decision.",
        keywords: &["tachi", "compact", "briefing", "usage", "summary"],
        evidence_state: "reference",
    },
    Seed {
        id: PENDING,
        text: "MCP skills adapter PR is pending merge. Implementation is on a work branch; acceptance has not started. The pending skills PR is not included in the runtime deployment. MCP skills 适配 PR 仍待合并，尚未验收，也未部署。",
        keywords: &["mcp", "skills", "adapter", "pr", "pending", "merge"],
        evidence_state: "pending_pr",
    },
    Seed {
        id: REVIEWED,
        text: "MCP skills adapter cold review completed on a fixed candidate. This receipt proves reviewed source only. Runtime acceptance and deployment were not performed by the review. MCP skills 冷审已完成，只审了代码，没有运行验收或部署。",
        keywords: &["mcp", "skills", "adapter", "review", "reviewed", "receipt"],
        evidence_state: "reviewed_only",
    },
    Seed {
        id: DEPLOYED,
        text: "MCP runtime deployment receipt: the previous stable daemon was installed and restarted; health and a live tool call passed. This deployed runtime predates the pending skills adapter PR. MCP runtime 已部署的是上一版稳定 daemon，健康检查和现场工具调用通过，不包含待合并的 skills PR。",
        keywords: &["mcp", "runtime", "deployment", "deployed", "daemon", "receipt"],
        evidence_state: "deployed_runtime",
    },
    Seed {
        id: "eq-mcp-reference",
        text: "MCP skills reference explains tool schemas and adapter configuration. A deployment guide lists build and restart commands; a guide is not a record that those commands were executed.",
        keywords: &["mcp", "skills", "adapter", "deployment", "reference"],
        evidence_state: "reference",
    },
    Seed {
        id: "eq-sqlite-migration",
        text: "SQLite schema migration rehearsal used a temporary fixture and rollback verification. The migration acceptance receipt belongs to the schema rehearsal, not to a runtime rollout.",
        keywords: &["sqlite", "schema", "migration", "acceptance", "receipt"],
        evidence_state: "unrelated_rehearsal",
    },
    Seed {
        id: "eq-recall-benchmark",
        text: "Tachi recall benchmark compares a fixed query corpus with expected record IDs. Measure missing hits, irrelevant results, rank and stale evidence before claiming recall quality improved.",
        keywords: &["tachi", "recall", "benchmark", "query", "evidence"],
        evidence_state: "reference",
    },
    Seed {
        id: "eq-health-note",
        text: "Tachi health check reports daemon availability. A healthy process alone does not establish which engineering change was accepted or why the implementation changed.",
        keywords: &["tachi", "health", "daemon", "engineering"],
        evidence_state: "reference",
    },
    Seed {
        id: "eq-ui-review",
        text: "Frontend status page review checked typography and empty-state layout. The review did not publish the website.",
        keywords: &["frontend", "review", "status", "layout"],
        evidence_state: "unrelated_review",
    },
    Seed {
        id: "eq-build-note",
        text: "Compiler cache ownership uses a serialized build seat. Build logs belong to the exact source candidate and cannot be substituted for acceptance evidence.",
        keywords: &["compiler", "cache", "build", "acceptance", "evidence"],
        evidence_state: "reference",
    },
];

struct Case {
    name: &'static str,
    query: &'static str,
    expected: &'static [&'static str],
    max_rank: usize,
    max_noise_at_5: usize,
}

const CASES: &[Case] = &[
    Case {
        name: "why-compact-briefing-changed",
        query: "Why did Tachi change compact briefing and what reason should a resumed session recover?",
        expected: &[CURRENT],
        max_rank: 1,
        max_noise_at_5: 4,
    },
    Case {
        name: "long-cjk-engineering-recall",
        query: "我记得之前讨论过 Tachi 的精简简报，但现在只记得最终结论，想找回当时为什么要调整、旧摘要漏掉了什么，以及恢复会话时应该保留哪些决策理由和证据来源，帮我把那条工程决策找出来。",
        expected: &[CURRENT],
        max_rank: 1,
        max_noise_at_5: 0,
    },
    Case {
        name: "what-actually-deployed",
        query: "Which MCP runtime was actually deployed and verified by a live tool call?",
        expected: &[DEPLOYED],
        max_rank: 1,
        max_noise_at_5: 4,
    },
    Case {
        name: "what-was-only-reviewed",
        query: "Which MCP skills adapter evidence only records a completed cold review of source?",
        expected: &[REVIEWED],
        max_rank: 1,
        max_noise_at_5: 4,
    },
    Case {
        name: "pending-pr-is-not-deployed",
        query: "MCP skills adapter PR pending merge acceptance not started",
        expected: &[PENDING],
        max_rank: 1,
        max_noise_at_5: 4,
    },
    Case {
        name: "compare-engineering-delivery-states",
        query: "MCP skills adapter PR pending merge reviewed source runtime deployed receipt",
        expected: &[PENDING, REVIEWED, DEPLOYED],
        max_rank: 3,
        max_noise_at_5: 2,
    },
    Case {
        name: "current-not-superseded-rationale",
        query: "Tachi compact briefing current rationale reason evidence",
        expected: &[CURRENT],
        max_rank: 1,
        max_noise_at_5: 2,
    },
    Case {
        name: "missing-migration-identifier",
        query: "QuartzLedgerCutover",
        expected: &[],
        max_rank: 0,
        max_noise_at_5: 0,
    },
    Case {
        name: "missing-acceptance-receipt",
        query: "7f298071-823a-4972-94cc-a57f1bd630ae",
        expected: &[],
        max_rank: 0,
        max_noise_at_5: 0,
    },
];

fn seed_corpus(conn: &mut Connection) {
    for (index, seed) in SEEDS.iter().enumerate() {
        let mut entry = memory_entry(seed.id, seed.text, seed.keywords);
        entry.path = format!("/notes/engineering-corpus/{}", seed.id);
        entry.source = SOURCE.into();
        entry.timestamp = "2026-06-01T00:00:00Z".into();
        entry.summary = seed.text.into();
        entry.metadata = json!({
            "keywords": seed.keywords,
            "synthetic_fixture": true,
            "evidence_state": seed.evidence_state,
            "fixture_index": index,
        });
        insert_entry(conn, entry);
    }
    crate::db::supersede_memory(conn, STALE, CURRENT).unwrap();
}

fn options() -> SearchOptions {
    SearchOptions {
        top_k: 5,
        record_access: false,
        mmr_threshold: None,
        ..Default::default()
    }
}

#[derive(Debug)]
struct Metrics {
    ranks: Vec<(&'static str, Option<usize>)>,
    misses: usize,
    noise_at_5: usize,
    stale: usize,
}

fn measure(ids: &[&str], expected: &'static [&'static str]) -> Metrics {
    let ranks: Vec<_> = expected
        .iter()
        .map(|id| (*id, ids.iter().position(|found| found == id).map(|i| i + 1)))
        .collect();
    Metrics {
        misses: ranks.iter().filter(|(_, rank)| rank.is_none()).count(),
        ranks,
        noise_at_5: ids
            .iter()
            .take(5)
            .filter(|id| !expected.contains(id))
            .count(),
        stale: ids.iter().filter(|id| **id == STALE).count(),
    }
}

#[test]
fn engineering_query_corpus_recall_floors() {
    let mut conn = setup();
    seed_corpus(&mut conn);
    for case in CASES {
        let rows = hybrid_search(&conn, case.query, &options()).unwrap();
        let ids: Vec<_> = rows.iter().map(|row| row.entry.id.as_str()).collect();
        let metrics = measure(&ids, case.expected);
        println!(
            "{} expected={:?} {metrics:?} ids={ids:?}",
            case.name, case.expected
        );
        assert_eq!(metrics.misses, 0, "{}: {metrics:?}; {ids:?}", case.name);
        for (_, rank) in &metrics.ranks {
            assert!(
                rank.is_some_and(|rank| rank <= case.max_rank),
                "{}: {metrics:?}; {ids:?}",
                case.name
            );
        }
        assert!(
            metrics.noise_at_5 <= case.max_noise_at_5,
            "{}: {metrics:?}; {ids:?}",
            case.name
        );
        assert_eq!(metrics.stale, 0, "{}: {metrics:?}; {ids:?}", case.name);
        for row in &rows {
            let seed = SEEDS.iter().find(|seed| seed.id == row.entry.id).unwrap();
            assert_eq!(row.entry.source, SOURCE);
            assert_eq!(row.entry.metadata["synthetic_fixture"], true);
            assert_eq!(row.entry.metadata["evidence_state"], seed.evidence_state);
        }
    }
}

#[test]
fn engineering_query_corpus_superseded_record_is_a_live_control() {
    let mut conn = setup();
    seed_corpus(&mut conn);
    let query = "Tachi compact briefing earlier rationale final decision";
    let history = hybrid_search(
        &conn,
        query,
        &SearchOptions {
            include_superseded: true,
            ..options()
        },
    )
    .unwrap();
    assert!(
        history.iter().any(|row| row.entry.id == STALE),
        "historical control was not retrieved: {history:?}"
    );
    let current = hybrid_search(&conn, query, &options()).unwrap();
    assert!(current.iter().any(|row| row.entry.id == CURRENT));
    assert!(!current.iter().any(|row| row.entry.id == STALE));
}

#[test]
fn engineering_query_corpus_fabricated_premise_reports_nearby_noise() {
    let mut conn = setup();
    seed_corpus(&mut conn);
    let rows = hybrid_search(
        &conn,
        "Why did Tachi finish the QuartzLedgerCutover migration and where is its acceptance receipt?",
        &options(),
    ).unwrap();
    let ids: Vec<_> = rows.iter().map(|row| row.entry.id.as_str()).collect();
    let metrics = measure(&ids, &[]);
    println!("fabricated-premise expected=[] {metrics:?} ids={ids:?}");
    // Retrieval can return lexical neighbours. None is evidence for the
    // invented cutover; the strong absent-identifier controls above must empty.
    assert!(rows
        .iter()
        .all(|row| !row.entry.text.contains("QuartzLedgerCutover")));
    assert_eq!(metrics.stale, 0);
}

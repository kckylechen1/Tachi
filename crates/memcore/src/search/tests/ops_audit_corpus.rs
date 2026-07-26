//! Operational recall discrimination corpus — permanent red baselines for the
//! 2026-07-09 memory ops audit (tachi#897 / epic #896 Phase 0, additive to the
//! #708 golden corpus in `golden_corpus.rs`).
//!
//! # What this is
//! Fully synthetic fixtures (no personal memory.db / host-local UUIDs) that
//! encode three measured defect classes from the ops audit:
//!
//! 1. **Same-store rank dilution** (single-DB model of the cross-library
//!    failure mode): a recent *project decision* buried under older
//!    roadmap/review noise for a task-shaped query. True global-vs-project
//!    merge lives in the tachi-server multi-DB suite
//!    (`ops_audit_discrimination`); this case freezes the rank-dilution shape
//!    inside pure hybrid ranking.
//! 2. **Adjacent wiki steal → surface split**: a labeled *research note*
//!    (owner-ratified `Surface::Memory`) competing with denser architecture
//!    wikis (`Surface::Docs`). Phase 2 dissolves the steal by scoping the
//!    query to Memory — the Docs wikis are excluded, so the note surfaces on
//!    relevance rather than needing a research-path magic multiplier (retired).
//!    Phase 2 P3 restores it to RANK 1 within Memory: the topical-evidence gate
//!    withholds DECISION_BOOST from the zero-evidence decision seed that used to
//!    sit above it.
//! 3. **Governance miss@10**: task framing / governance decision missing from
//!    top-10 when component-registry stubs and old roadmap dominate.
//!
//! # Assertion layers (post same-store precision fix + Phase 2 surface split + P3 gate)
//! - **Ratchet / product layers** (plain `#[test]`): rank-1 for the decision
//!   (fused pool); rank-1 for the research note within `Surface::Memory`
//!   (above the registry-stub noise — P3's evidence gate drops the zero-evidence
//!   DECISION_BOOST that used to outrank it); hit@5 for governance. Pre-fix red
//!   baseline was ranks 6 / 7 / miss@10 — do not re-introduce those shapes.
//! - **Report** (`#[ignore]` diagnostic): prints ranks for floor refresh.
//!
//! # Determinism
//! Fixed base timestamps (`BASE − days_ago`), no `Utc::now()` for entry ages,
//! MMR disabled, `record_access: false`. Production stable tie-break
//! (tachi#718) keeps id order byte-stable.

use super::*;
use crate::namespace::Surface;

fn ts_days_ago(days_ago: i64) -> String {
    let base = chrono::DateTime::parse_from_rfc3339("2026-06-01T00:00:00+00:00").unwrap();
    (base - chrono::Duration::days(days_ago)).to_rfc3339()
}

struct Seed {
    id: &'static str,
    path: &'static str,
    category: &'static str,
    text: &'static str,
    keywords: &'static [&'static str],
    entities: &'static [&'static str],
    importance: f64,
    days_ago: i64,
    tier: &'static str,
}

/// Synthetic corpus inventing the three ops-audit defect shapes. All text is
/// fabricated; nothing is a real host memory.
const SEEDS: &[Seed] = &[
    // ---- Case 1: task-shaped decision buried under roadmap/review noise ----
    Seed {
        id: "ops-project-decision-priority",
        path: "/notes/project/decisions",
        category: "decision",
        text: "Project decision for this sprint: open issue priority is binding receipts first then ranking discrimination suite before digest work",
        keywords: &["project", "decision", "open", "issue", "priority", "sprint"],
        entities: &["BindingReceipts", "OpsAudit"],
        importance: 0.95,
        days_ago: 2,
        tier: "raw",
    },
    Seed {
        id: "ops-global-roadmap-memory-os",
        path: "/wiki/memory-operating-system",
        category: "wiki",
        text: "Memory operating system roadmap covers bind rank digest graph and soul phases including open issue priority queues for project decisions across the campaign",
        keywords: &["memory", "roadmap", "open", "issue", "priority", "project", "decision", "rank"],
        entities: &["MemoryOS"],
        importance: 0.85,
        days_ago: 90,
        tier: "consolidated",
    },
    Seed {
        id: "ops-global-review-quarterly",
        path: "/wiki/quarterly-review",
        category: "wiki",
        text: "Quarterly architecture review of open issue triage prioritization and ranking quality for memory operations across every project decision surface",
        keywords: &["review", "open", "issue", "priority", "ranking", "project", "decision"],
        entities: &["QuarterlyReview"],
        importance: 0.8,
        days_ago: 60,
        tier: "consolidated",
    },
    Seed {
        id: "ops-global-review-ranking",
        path: "/guide/ranking-review",
        category: "guide",
        text: "Architecture review notes on open issue priority queues and ranking of project decisions in hybrid recall for the memory operating system",
        keywords: &["architecture", "review", "open", "issue", "priority", "ranking", "project"],
        entities: &[],
        importance: 0.75,
        days_ago: 45,
        tier: "raw",
    },
    Seed {
        id: "ops-global-roadmap-phase2",
        path: "/wiki/phase2-precision",
        category: "wiki",
        text: "Phase two precision floor requires rank one for recent project decisions and open issue priority judgments without global roadmap dilution",
        keywords: &["phase", "precision", "project", "decision", "open", "issue", "priority", "roadmap"],
        entities: &["Phase2"],
        importance: 0.8,
        days_ago: 40,
        tier: "consolidated",
    },
    // ---- Case 2: research note vs. adjacent architecture wikis ----
    // The labeled target is a *research note* — owner-ratified as
    // `Surface::Memory` (not a wiki entry, not `guide`-category). The three
    // architecture wikis below are genuine reference docs (`Surface::Docs`).
    // Phase 2 dissolves the old "adjacent wiki steal": scoping the query to
    // `Surface::Memory` excludes the Docs wikis entirely, so the research note
    // surfaces near the top WITHIN Memory — no research-path magic multiplier.
    // Its category is `research` and path is `/notes/...`, so `surface_of`
    // classifies it Memory (not `is_wiki_entry`, category != `guide`). The
    // Case-3 registry-stub rows (also `/notes/**`, Memory) supply the
    // discrimination noise this case must out-rank.
    Seed {
        id: "ops-wiki-research-hindsight",
        path: "/notes/research/hindsight-eval",
        category: "research",
        text: "Hindsight study findings: a short research note on how agents mis-rank recent decisions when older architecture pages agree with each other",
        keywords: &["hindsight", "study", "findings"],
        entities: &["Hindsight"],
        importance: 0.7,
        days_ago: 12,
        tier: "raw",
    },
    Seed {
        id: "ops-wiki-arch-recall-quality",
        path: "/wiki/architecture/recall-quality",
        category: "wiki",
        text: "Recall quality architecture: memory recall quality evaluation protocol for research discrimination cases, golden corpus harness, and hindsight-style labeled checks that gate ranking changes",
        keywords: &[
            "recall",
            "quality",
            "architecture",
            "memory",
            "evaluation",
            "protocol",
            "research",
            "discrimination",
            "hindsight",
            "labeled",
            "golden",
        ],
        entities: &["RecallQuality", "Hindsight"],
        importance: 0.9,
        days_ago: 30,
        tier: "pattern",
    },
    Seed {
        id: "ops-wiki-arch-hybrid",
        path: "/wiki/architecture/hybrid-recall",
        category: "wiki",
        text: "Hybrid recall architecture for memory recall quality evaluation protocol research: vector full text symbolic fusion and hindsight agreement noise analysis",
        keywords: &[
            "hybrid",
            "recall",
            "architecture",
            "memory",
            "quality",
            "evaluation",
            "protocol",
            "research",
            "hindsight",
        ],
        entities: &["Hindsight"],
        importance: 0.88,
        days_ago: 28,
        tier: "consolidated",
    },
    Seed {
        id: "ops-wiki-arch-eval-harness",
        path: "/wiki/architecture/eval-harness",
        category: "wiki",
        text: "Evaluation harness architecture wiki documenting memory recall quality research protocol discrimination targets and hindsight regression tables",
        keywords: &[
            "evaluation",
            "harness",
            "memory",
            "recall",
            "quality",
            "research",
            "protocol",
            "hindsight",
        ],
        entities: &["Hindsight"],
        importance: 0.86,
        days_ago: 25,
        tier: "consolidated",
    },
    // ---- Case 3: governance framing missed under registry stubs + roadmap ----
    // Expected uses adjudicated-decision vocabulary; the task-shaped query
    // leans on "component registry" tokens that the stubs and old roadmaps own.
    Seed {
        id: "ops-gov-framing-cutover",
        path: "/notes/project/governance",
        category: "decision",
        text: "Owner ratified stance: freeze the task taxonomy before any promote step, refuse silent merges of false-friend libraries, and demand an adjudicated apply path rather than bulk copy",
        keywords: &["taxonomy", "adjudicated", "apply", "false-friend"],
        entities: &["OwnerStance"],
        importance: 0.92,
        days_ago: 5,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-router",
        path: "/notes/registry/router",
        category: "fact",
        text: "Component registry stub router: component registry cutover inventory row for router surface in the component registry catalog",
        keywords: &["component", "registry", "cutover", "router", "stub", "inventory"],
        entities: &["Router", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 100,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-store",
        path: "/notes/registry/store",
        category: "fact",
        text: "Component registry stub store: component registry cutover inventory row for store adapters in the component registry catalog",
        keywords: &["component", "registry", "cutover", "store", "stub", "inventory"],
        entities: &["Store", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 99,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-search",
        path: "/notes/registry/search",
        category: "fact",
        text: "Component registry stub search: component registry cutover inventory row for search entry points in the component registry catalog",
        keywords: &["component", "registry", "cutover", "search", "stub", "inventory"],
        entities: &["Search", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 98,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-vault",
        path: "/notes/registry/vault",
        category: "fact",
        text: "Component registry stub vault: component registry cutover inventory row for vault leases in the component registry catalog",
        keywords: &["component", "registry", "cutover", "vault", "stub", "inventory"],
        entities: &["Vault", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 97,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-foundry",
        path: "/notes/registry/foundry",
        category: "fact",
        text: "Component registry stub foundry: component registry cutover inventory row for foundry distill in the component registry catalog",
        keywords: &["component", "registry", "cutover", "foundry", "stub", "inventory"],
        entities: &["Foundry", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 96,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-hub",
        path: "/notes/registry/hub",
        category: "fact",
        text: "Component registry stub hub: component registry cutover inventory row for hub capabilities in the component registry catalog",
        keywords: &["component", "registry", "cutover", "hub", "stub", "inventory"],
        entities: &["Hub", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 95,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-profile",
        path: "/notes/registry/profile",
        category: "fact",
        text: "Component registry stub profile: component registry cutover inventory row for profile identity in the component registry catalog",
        keywords: &["component", "registry", "cutover", "profile", "stub", "inventory"],
        entities: &["Profile", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 94,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-sandbox",
        path: "/notes/registry/sandbox",
        category: "fact",
        text: "Component registry stub sandbox: component registry cutover inventory row for sandbox rules in the component registry catalog",
        keywords: &["component", "registry", "cutover", "sandbox", "stub", "inventory"],
        entities: &["Sandbox", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 93,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-wiki",
        path: "/notes/registry/wiki",
        category: "fact",
        text: "Component registry stub wiki: component registry cutover inventory row for wiki pages in the component registry catalog",
        keywords: &["component", "registry", "cutover", "wiki", "stub", "inventory"],
        entities: &["Wiki", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 92,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-dispatch",
        path: "/notes/registry/dispatch",
        category: "fact",
        text: "Component registry stub dispatch: component registry cutover inventory row for dispatch lanes in the component registry catalog",
        keywords: &["component", "registry", "cutover", "dispatch", "stub", "inventory"],
        entities: &["Dispatch", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 91,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-facade",
        path: "/notes/registry/facade",
        category: "fact",
        text: "Component registry stub facade: component registry cutover inventory row for facade tools in the component registry catalog",
        keywords: &["component", "registry", "cutover", "facade", "stub", "inventory"],
        entities: &["Facade", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 90,
        tier: "raw",
    },
    Seed {
        id: "ops-registry-stub-daemon",
        path: "/notes/registry/daemon",
        category: "fact",
        text: "Component registry stub daemon: component registry cutover inventory row for daemon binding in the component registry catalog",
        keywords: &["component", "registry", "cutover", "daemon", "stub", "inventory"],
        entities: &["Daemon", "ComponentRegistry"],
        importance: 0.55,
        days_ago: 89,
        tier: "raw",
    },
    Seed {
        id: "ops-roadmap-old-registry",
        path: "/wiki/roadmap/component-registry",
        category: "wiki",
        text: "Old roadmap for component registry cutover: enumerate every component registry stub in the inventory then migrate storage without owner stance or adjudicated apply",
        keywords: &["roadmap", "component", "registry", "cutover", "stub", "inventory"],
        entities: &["ComponentRegistry"],
        importance: 0.8,
        days_ago: 120,
        tier: "pattern",
    },
    Seed {
        id: "ops-roadmap-old-cutover",
        path: "/wiki/roadmap/cutover-plan",
        category: "wiki",
        text: "Legacy component registry cutover plan listing component registry stub inventory ownership and migration order without task framing",
        keywords: &["cutover", "component", "registry", "migration", "stub", "inventory"],
        entities: &["ComponentRegistry"],
        importance: 0.78,
        days_ago: 110,
        tier: "consolidated",
    },
    Seed {
        id: "ops-roadmap-old-governance-noise",
        path: "/wiki/roadmap/governance-noise",
        category: "wiki",
        text: "Roadmap noise page about component registry cutover governance checklists that only restate stub inventory status for the component registry program",
        keywords: &["roadmap", "component", "registry", "cutover", "governance", "stub"],
        entities: &["ComponentRegistry"],
        importance: 0.76,
        days_ago: 105,
        tier: "consolidated",
    },
    // Extra high-agreement noise so weak paraphrase matches cannot sit in top-10
    // on a tiny corpus (everything would otherwise fit in top-10).
    Seed {
        id: "ops-noise-gov-01",
        path: "/wiki/noise/component-registry-a",
        category: "wiki",
        text: "Component registry cutover governance checklist A: stubs dominate the inventory while old roadmap rows still frame the component registry program status",
        keywords: &["component", "registry", "cutover", "governance", "stubs", "roadmap", "frame"],
        entities: &["ComponentRegistry"],
        importance: 0.82,
        days_ago: 80,
        tier: "consolidated",
    },
    Seed {
        id: "ops-noise-gov-02",
        path: "/wiki/noise/component-registry-b",
        category: "wiki",
        text: "Component registry cutover governance checklist B: when stubs and old roadmap dominate the component registry framing notes stay inventory-only",
        keywords: &["component", "registry", "cutover", "governance", "stubs", "roadmap", "dominate"],
        entities: &["ComponentRegistry"],
        importance: 0.82,
        days_ago: 79,
        tier: "consolidated",
    },
    Seed {
        id: "ops-noise-gov-03",
        path: "/wiki/noise/component-registry-c",
        category: "wiki",
        text: "Component registry cutover governance checklist C: framing for component registry cutover is restated as stub ownership tables and old roadmap bullets",
        keywords: &["component", "registry", "cutover", "governance", "framing", "stub", "roadmap"],
        entities: &["ComponentRegistry"],
        importance: 0.82,
        days_ago: 78,
        tier: "consolidated",
    },
    Seed {
        id: "ops-noise-gov-04",
        path: "/wiki/noise/component-registry-d",
        category: "wiki",
        text: "Component registry cutover governance checklist D: stubs dominate old roadmap pages that still say governance framing without an owner stance",
        keywords: &["component", "registry", "cutover", "governance", "framing", "stubs", "roadmap"],
        entities: &["ComponentRegistry"],
        importance: 0.81,
        days_ago: 77,
        tier: "consolidated",
    },
    Seed {
        id: "ops-noise-gov-05",
        path: "/wiki/noise/component-registry-e",
        category: "wiki",
        text: "Component registry cutover governance checklist E: old roadmap and stub inventory dominate every component registry cutover governance search",
        keywords: &["component", "registry", "cutover", "governance", "roadmap", "stub", "dominate"],
        entities: &["ComponentRegistry"],
        importance: 0.81,
        days_ago: 76,
        tier: "consolidated",
    },
    Seed {
        id: "ops-noise-gov-06",
        path: "/wiki/noise/component-registry-f",
        category: "wiki",
        text: "Component registry cutover governance checklist F: framing notes for component registry cutover when stubs dominate remain catalog noise",
        keywords: &["component", "registry", "cutover", "governance", "framing", "stubs", "dominate"],
        entities: &["ComponentRegistry"],
        importance: 0.8,
        days_ago: 75,
        tier: "consolidated",
    },
    Seed {
        id: "ops-noise-gov-07",
        path: "/wiki/noise/component-registry-g",
        category: "wiki",
        text: "Component registry cutover governance checklist G: old roadmap rows about component registry cutover governance framing fill the head of lexical recall",
        keywords: &["component", "registry", "cutover", "governance", "framing", "roadmap"],
        entities: &["ComponentRegistry"],
        importance: 0.8,
        days_ago: 74,
        tier: "consolidated",
    },
    Seed {
        id: "ops-noise-gov-08",
        path: "/wiki/noise/component-registry-h",
        category: "wiki",
        text: "Component registry cutover governance checklist H: stubs and old roadmap dominate component registry cutover governance framing queries",
        keywords: &["component", "registry", "cutover", "governance", "framing", "stubs", "roadmap", "dominate"],
        entities: &["ComponentRegistry"],
        importance: 0.8,
        days_ago: 73,
        tier: "consolidated",
    },
];

/// `pub(super)`: reused by `search/tests/rank_attribution.rs` (tachi#1344
/// boost-attribution harness) so it seeds the byte-identical corpus rather
/// than inventing a parallel one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DefectClass {
    /// Recent project decision buried under older roadmap/review noise.
    RankDilution,
    /// Labeled research note (Memory surface) must land RANK 1 in its surface,
    /// above the registry-stub noise; the adjacent architecture wikis are Docs
    /// and excluded by surface scoping. (Rank-1 restored in P3: the
    /// topical-evidence gate drops the zero-evidence DECISION_BOOST that used to
    /// outrank the note within Memory.)
    AdjacentWikiSteal,
    /// Governance framing missing from top-10 under registry stubs.
    GovernanceMiss,
}

pub(super) struct CaseSpec {
    pub(super) class: DefectClass,
    pub(super) name: &'static str,
    pub(super) query: &'static str,
    pub(super) expected: &'static str,
    /// Retrieval surface to scope the query to. `None` = today's fused pool
    /// (unchanged behavior). The `hindsight-research-wiki` case runs under
    /// `Some(Surface::Memory)` so the Docs architecture wikis are excluded and
    /// the research note wins on relevance within Memory (Phase 2 dissolution
    /// of the research-path boost).
    pub(super) surface: Option<Surface>,
}

pub(super) const CASES: &[CaseSpec] = &[
    CaseSpec {
        class: DefectClass::RankDilution,
        name: "open-issue-priority-decision",
        query: "what is the current open issue priority project decision for this sprint",
        expected: "ops-project-decision-priority",
        surface: None,
    },
    CaseSpec {
        class: DefectClass::AdjacentWikiSteal,
        name: "hindsight-research-wiki",
        // Research-shaped query scoped to Memory: the Docs architecture wikis
        // that used to steal rank 1 are excluded by the surface filter, so the
        // labeled research note surfaces within Memory — no research-path boost
        // needed. P3 makes it RANK 1: the topical-evidence gate withholds
        // DECISION_BOOST from the zero-evidence decision seed that used to sit
        // above it (see the ratchet arm).
        query: "hindsight research evaluation protocol for memory recall quality",
        expected: "ops-wiki-research-hindsight",
        surface: Some(Surface::Memory),
    },
    CaseSpec {
        class: DefectClass::GovernanceMiss,
        name: "governance-framing-cutover",
        // Paraphrase leans on component-registry tokens the stubs own; the
        // expected decision uses adjudicated/taxonomy vocabulary instead.
        query:
            "governance framing for component registry cutover when stubs and old roadmap dominate",
        expected: "ops-gov-framing-cutover",
        surface: None,
    },
];

fn seed_entry(s: &Seed) -> MemoryEntry {
    let mut e = memory_entry(s.id, s.text, s.keywords);
    e.path = s.path.into();
    e.category = s.category.into();
    e.entities = s.entities.iter().map(|x| x.to_string()).collect();
    e.importance = s.importance;
    e.summary = s.text.chars().take(60).collect();
    e.timestamp = ts_days_ago(s.days_ago);
    e.tier = s.tier.into();
    e.metadata = json!({ "keywords": s.keywords, "entities": s.entities });
    e
}

// `pub(super)` so the P3 Step-0 probe (`p3_probe.rs`) and the #1344 boost
// attribution harness (`rank_attribution.rs`) reuse the EXACT same corpus +
// search options as this ratchet — a probe must measure the real post-P2
// ranking on the identical fixture, not a divergent copy. Test-only
// visibility; no runtime behavior changes.
pub(super) fn seed_corpus(conn: &mut Connection) {
    for s in SEEDS {
        insert_entry(conn, seed_entry(s));
    }
}

pub(super) fn search_opts(surface: Option<Surface>) -> SearchOptions {
    SearchOptions {
        top_k: 40,
        candidates_per_channel: 128,
        record_access: false,
        mmr_threshold: None,
        surface,
        ..Default::default()
    }
}

/// 1-based rank of `expected` under production order (no test-side re-sort),
/// or `None` if absent from the returned pool.
fn rank_of(
    conn: &Connection,
    query: &str,
    expected: &str,
    surface: Option<Surface>,
) -> Option<usize> {
    let results = hybrid_search(conn, query, &search_opts(surface)).unwrap();
    results
        .iter()
        .position(|r| r.entry.id == expected)
        .map(|i| i + 1)
}

fn returned_ids(conn: &Connection, query: &str, surface: Option<Surface>) -> Vec<String> {
    hybrid_search(conn, query, &search_opts(surface))
        .unwrap()
        .into_iter()
        .map(|r| r.entry.id)
        .collect()
}

// ---------------------------------------------------------------------------
// Floors after same-store precision (#708 Phase D decision boost), the Phase 2
// surface split, and the Phase 2 P3 topical-evidence gate. Pre-fix red baseline
// (report on pre-#708 branch): ranks 6 / 7 / miss@10. The research case now
// ratchets RANK 1 WITHIN `Surface::Memory` (above the registry-stub noise; the
// research-path boost was retired) — P3's evidence gate withholds
// DECISION_BOOST from the zero-evidence decision seed that used to sit above
// it, so the genuinely-retrieved note wins on relevance alone. The decision and
// governance cases stay on the fused pool (`surface: None`) with real topical
// evidence, so they retain their boost. Floors ratchet upward per their case
// surface — never re-introduce the buried shapes.
// ---------------------------------------------------------------------------

/// RATCHET LAYER — green after the same-store precision fix + surface split +
/// P3 evidence gate. Locks product ranks so regressions re-burying
/// decisions/research turn CI red. Each case is scored under its own `surface`
/// scope (see each arm for the exact criterion — rank-1 for dilution, rank-1
/// within Memory for the research note, hit@5 for governance).
#[test]
fn ops_audit_corpus_ratchet_floors() {
    let mut conn = setup();
    seed_corpus(&mut conn);

    for case in CASES {
        let rank = rank_of(&conn, case.query, case.expected, case.surface);
        let ids = returned_ids(&conn, case.query, case.surface);
        let top10: Vec<&str> = ids.iter().take(10).map(String::as_str).collect();

        match case.class {
            DefectClass::RankDilution => {
                assert_eq!(
                    rank,
                    Some(1),
                    "case `{}`: expected `{}` at rank 1 (post #708 same-store precision), got {rank:?}; top10={top10:?}",
                    case.name,
                    case.expected
                );
            }
            DefectClass::AdjacentWikiSteal => {
                // P3 deliverable: the labeled research note is now RANK 1 within
                // its Memory surface. Previously it sat at rank 2 behind
                // `ops-project-decision-priority`, which reached rank 2 purely on
                // the un-capped DECISION_BOOST × the recency tie-break, with ZERO
                // topical evidence (its only symbolic score is the dense-map
                // noise floor of a single shared token). The P3 topical-evidence
                // gate (`ranking.rs::has_topical_evidence`) withholds
                // DECISION_BOOST from that zero-evidence decision seed, so the
                // genuinely-retrieved research note takes rank 1 on relevance —
                // no compensating research-path multiplier. This is the tightest
                // form of the invariant: an exact rank-1, not a top-3 band.
                assert_eq!(
                    rank,
                    Some(1),
                    "case `{}`: expected `{}` at rank 1 within Surface::Memory \
                     (P3 evidence gate drops the zero-evidence DECISION_BOOST that \
                     used to outrank the research note), got rank={rank:?}; top10={top10:?}",
                    case.name,
                    case.expected
                );
                // The `ops-registry-stub-*` rows are `/notes/**` (Memory) and
                // ARE retrieved into this query's pool (confirmed at runtime —
                // they appeared below the note in the observed top-N), so this
                // loop is a live "above every stub" check, not vacuous.
                let expected_pos = ids.iter().position(|id| id == case.expected);
                for (i, id) in ids.iter().enumerate() {
                    if id.starts_with("ops-registry-stub-") {
                        assert!(
                            expected_pos.is_some_and(|p| p < i),
                            "case `{}`: `{}` (pos {expected_pos:?}) must rank above \
                             registry-stub noise row `{id}` (pos {i}) within Memory; \
                             top10={top10:?}",
                            case.name,
                            case.expected
                        );
                    }
                }
            }
            DefectClass::GovernanceMiss => {
                assert!(
                    matches!(rank, Some(r) if r <= 5),
                    "governance case `{}`: expected `{}` hit@5 (post fix), got rank={rank:?}; top10={top10:?}",
                    case.name,
                    case.expected
                );
            }
        }
    }
}

/// Companion to the ratchet's `hindsight-research-wiki` case: the SAME query
/// scoped to `Surface::Docs` returns an architecture wiki at rank 1 and drops
/// the research note entirely. Documents that the Phase 2 split sends the
/// wikis to their own surface — the note and the wikis no longer compete in
/// one pool, which is what retired the research-path boost.
#[test]
fn hindsight_query_under_docs_surface_tops_an_architecture_wiki() {
    let mut conn = setup();
    seed_corpus(&mut conn);

    let query = "hindsight research evaluation protocol for memory recall quality";
    let ids = returned_ids(&conn, query, Some(Surface::Docs));
    let top = ids.first().map(String::as_str);

    assert!(
        matches!(
            top,
            Some(
                "ops-wiki-arch-recall-quality"
                    | "ops-wiki-arch-hybrid"
                    | "ops-wiki-arch-eval-harness"
            )
        ),
        "expected an architecture wiki at rank 1 under Surface::Docs, got {top:?}; ids={ids:?}"
    );
    assert!(
        !ids.contains(&"ops-wiki-research-hindsight".to_string()),
        "research note is Surface::Memory and must NOT appear under Docs; got {ids:?}"
    );
}

/// Determinism lock — identical query → identical id order across reruns.
#[test]
fn ops_audit_corpus_recall_order_is_deterministic() {
    let mut conn = setup();
    seed_corpus(&mut conn);

    const N: usize = 8;
    for case in CASES {
        let baseline = returned_ids(&conn, case.query, case.surface);
        for run in 1..N {
            let ids = returned_ids(&conn, case.query, case.surface);
            assert_eq!(
                ids, baseline,
                "case `{}` nondeterministic on run {run}/{N}",
                case.name
            );
        }
    }
}

/// REPORT — not a gate. Prints per-case rank + top-5 for floor refresh.
#[test]
#[ignore = "diagnostic report only — run with --ignored --nocapture"]
fn ops_audit_corpus_report() {
    let mut conn = setup();
    seed_corpus(&mut conn);

    println!("ops_audit_corpus report (tachi#897)");
    for case in CASES {
        let rank = rank_of(&conn, case.query, case.expected, case.surface);
        let ids = returned_ids(&conn, case.query, case.surface);
        let top5: Vec<&str> = ids.iter().take(5).map(String::as_str).collect();
        println!(
            "  [{:?}] {} expected={} rank={:?} top5={:?}",
            case.class, case.name, case.expected, rank, top5
        );
    }
}

/// #1413 concern 3: regression floor for the `hindsight-research-wiki` case
/// under the FUSED pool (`surface=None`). Unlike the ratchet arm above — which
/// locks the research note at RANK 1 WITHIN `Surface::Memory` (where the
/// architecture wikis are excluded) — this guard runs the SAME query on the
/// fused pool, where the denser Docs architecture wikis compete directly with
/// the note. It asserts ONLY a hit@10 floor: the note must stay retrievable in
/// the top-10, with NO rank boost applied. It is a pure regression guard —
/// surface=None applies no surface predicate, so this exercises pre-existing
/// fused ranking that is untouched by the graph-expansion cross-surface fix
/// (which filters only when a surface is explicitly requested).
#[test]
fn hindsight_research_note_remains_hit_at_10_under_fused_pool() {
    let mut conn = setup();
    seed_corpus(&mut conn);

    let query = "hindsight research evaluation protocol for memory recall quality";
    let rank = rank_of(&conn, query, "ops-wiki-research-hindsight", None);
    let ids = returned_ids(&conn, query, None);
    let top10: Vec<&str> = ids.iter().take(10).map(String::as_str).collect();

    assert!(
        matches!(rank, Some(r) if r <= 10),
        "hindsight research note must remain hit@10 under surface=None (fused pool); \
         got rank={rank:?}; top10={top10:?}"
    );
}

//! Exposure-loop magnitude harness — tachi#1446 commit 0 (measurement) plus
//! commit 1's RED/GREEN regression pair.
//!
//! # What this is
//! `hybrid_search` bumps an access record for **every row it returns**
//! (`search.rs:501` collects `results.iter().map(|r| r.entry.id)` and feeds
//! `record_access_with_updates`), and that record feeds ranking. So the act of
//! surfacing a memory can make it rank higher next time: exposure manufactures
//! rank. Issue #1446 hand-derived three levers and their magnitudes from the
//! formulas. This module converts those hand-derivations into corpus
//! measurements, so priority is fixed by evidence rather than arithmetic.
//!
//! **Nothing in commit 0 fixed the loop.** The three `measure_*` tests are
//! green-today instruments. The fourth test,
//! [`exposure_alone_must_not_change_rank`], is the regression gate for the fix
//! and is **RED on purpose** at default config (`#[ignore]`d, see its own doc).
//!
//! Commit 1 added the fifth test,
//! [`use_provenance_recency_keeps_exposure_out_of_rank`] — the same scenario
//! with `RecallConfig::use_provenance_recency` on, which is **GREEN**. The
//! RED/GREEN pair is the proof: the defect is real at shipped config, and that
//! one switch is what removes it. The default is unchanged, so the fourth test
//! stays red and stays ignored until the knob flips.
//!
//! # The three levers (verified at `e75478ed9`)
//!
//! | # | Lever | Source | Reaches the score via |
//! |---|---|---|---|
//! | L1 | `last_access` reset | `db/memory_crud/access.rs:115` — one UPDATE does `access_count = access_count + 1, last_access = ?1` | `recency` in `default_decay_score_with_config` (`scorer.rs:150`) → the additive decay term `blended + weights.decay * ds / rrf_k` (`scorer.rs:517`) |
//! | L2 | access-feedback multiplier | `search/ranking.rs:612-616` — `access_count >= 2` ⇒ `final_score *= min(1 + ln1p(n)*0.03, 1.25)` | multiplies the *final* score, so it is independent of `weights.decay` |
//! | L3 | durable tier ratchet | `db/memory_crud/access.rs:196-200` — `tier='raw' AND recall_count>=3 AND query_diversity>=3` ⇒ `consolidated` | half-life 30d→60d (`recall_config.rs:11-12`) **and** the `("consolidated", false) => 1.08` tier boost (`ranking.rs:634`) |
//!
//! Decay is **not** rank-fused — `rank_map` is built only for vec/fts/symbolic
//! (`scorer.rs:466-468`) — so an L1 change lands directly on the final score at
//! `scorer.rs:517`, scaled by `weights.decay / rrf_k`. That scaling is what the
//! two-profile contrast below is built to expose.
//!
//! # Why every measurement runs on two path profiles
//! L1's leverage is multiplied by `weights.decay` (0.20 default vs **0.02** on
//! `/guide` and `/wiki`, `recall_config.rs:50-56`); L2's is not. So a harness
//! that only ever runs the default profile cannot tell the two apart. Every
//! score identity asserted here is exact in closed form, which makes the
//! contrast a committed number rather than a narrative:
//!
//! * L1 `Δfinal == weights.decay * Δdecay / rrf_k` ⇒ the *same* mutation on the
//!   *same* candidate yields exactly `0.20 / 0.02 == 10×` more movement on the
//!   default profile than on `/guide`. Asserted in [`measure_l1_last_access_reset`].
//! * L2 `final_after / final_before == min(1 + ln1p(n)*0.03, 1.25)` ⇒ identical
//!   on both profiles. Asserted in [`measure_l2_access_feedback_multiplier`],
//!   which also asserts that at `decay = 0.02` L2 still out-moves L1 on the same
//!   candidate — the discriminating prediction.
//!
//! # How the levers are isolated (and the three traps #1446 named)
//!
//! **Trap 1 — env knobs are inert in-crate.** `RecallConfig::load` returns
//! `Self::default()` under `cfg!(test)` (`recall_config.rs:92`), so a
//! `TACHI_RECALL_*` env var would silently measure the default profile twice.
//! The `/guide` weights are therefore read from an explicit `RecallConfig` via
//! `weights_for_path` (`recall_config.rs:114-127`) and injected through
//! `SearchOptions` (`search.rs:83` `recall_config`, plus `weights`). See
//! [`Profile::weights`] for why `weights` and not `path_prefix`.
//!
//! **Trap 2 — `apply_quality_boosts` is set-relative.** Its floor is
//! `0.85 × ` the candidate-set max (`ranking.rs:312-317`), so in general a
//! counterfactual that moves one candidate can change which *other* candidates
//! qualify. This fixture **defuses** that instead of caveating it: every seed
//! scores `quality_multiplier == 1.0` exactly, so the
//! `(multiplier - 1.0).abs() > f64::EPSILON` guard at `ranking.rs:320` never
//! fires and the floor is never consulted. That is asserted, not assumed — see
//! [`assert_quality_boosts_are_inert`], called by every fixture build.
//!
//! By the same construction the other six boost steps are invariant across
//! every counterfactual here, so each measurement is genuinely per-lever:
//! * `apply_precision_boosts` — the query has whitespace ⇒
//!   `is_id_like_exact_query` is false (`scorer/text.rs:128-137`) ⇒ multiplier
//!   `1.0` (`scorer/text.rs:182-192`).
//! * `apply_quality_boosts` — inert, above.
//! * `apply_tier_boosts` — all seeds are `raw` ⇒ `1.0` (`ranking.rs:630-636`);
//!   this is the one step [`measure_l3_tier_ratchet`] deliberately turns on.
//! * `apply_entity_recency_boosts` — no seed carries entities, and
//!   `newest_by_shared_entity` only yields ids from entity buckets of size > 1
//!   (`filtering.rs:72-103`).
//! * `apply_decision_boost` — all seeds are `category = "fact"`, the gate needs
//!   `"decision"` (`ranking.rs:422-424`).
//! * `apply_lexical_overlap_boost` — every seed contains every query token, so
//!   each token's document frequency equals the candidate count and the
//!   discriminative-token gate (`freq * 4 <= entries_ref.len()`,
//!   `ranking.rs:512-522`) rejects all of them; the boost is withheld from the
//!   whole set.
//!
//! Net: the total multiplicative envelope is exactly `1.0`, which is what makes
//! the closed-form identities asserted below exact rather than approximate. If
//! any of that ever stops holding, those identity assertions are what fails
//! first — they are the harness's own smoke alarm.
//!
//! **Trap 3 — score ties.** #718/#722 removed tie-break nondeterminism with the
//! stable `(final_score desc, timestamp desc, id asc)` key
//! (`scorer.rs:353-360`). This fixture uses **both** halves of the packet's
//! choice, deliberately, and says which where:
//! * *Final* scores are distinct by construction — each seed lands on a distinct
//!   `(fts_rank, symbolic_rank)` pair and, above the importance floor, a
//!   distinct age.
//! * *Symbolic channel* scores do tie: `symbolic_score_entry` saturates at 1.0
//!   for every seed (all of them contain all three query tokens — which they
//!   must, or conjunctive FTS would drop them from the candidate set entirely).
//!   That tie is broken by the #718 key, i.e. by `timestamp desc`. This is
//!   leaned on knowingly: it is total and deterministic, so the symbolic rank
//!   order is reproducible run to run. No assertion here depends on *which*
//!   order it produces.
//!
//! # Determinism and the wall clock
//! Seed timestamps are anchored to `Utc::now() - days_ago`, not to a fixed
//! calendar instant. That is on purpose and is the opposite of
//! `golden_corpus`'s choice: `default_decay_score_with_config` reads
//! `Utc::now()` (`scorer.rs:135`), so a fixed-instant corpus would make every
//! decay magnitude drift as the file ages, and any committed number would rot.
//! Now-anchoring holds `age_days` constant, so the magnitudes asserted here are
//! stable. The residual within-test clock drift (a few ms between two
//! `hybrid_search` calls) moves a decay score by ~1e-10 relative; assertions on
//! *other* candidates' invariance therefore use [`CLOCK_TOLERANCE`], four or
//! more orders of magnitude below every effect being measured.
//!
//! # Corpus reuse
//! Seeded with the shared `search/tests.rs` helpers — `setup`, `memory_entry`,
//! `insert_entry` — the same ones `access.rs` and `decay_policy.rs` use, and
//! the same ones `golden_corpus::seed_entry` builds on. `golden_corpus` /
//! `ops_audit_corpus` themselves are *not* reused: both are pinned to a fixed
//! base instant (`golden_corpus.rs:60-64`) for reproducible ranking, which is
//! exactly the property that would make a decay-magnitude number rot (above),
//! and neither carries a freshness gradient with floored *and* unfloored
//! candidates in the same top-K, which is what a per-lever counterfactual needs.
//!
//! # How to run (Oz)
//!     cargo nextest run -p memcore measure_l1 measure_l2 measure_l3
//!     cargo nextest run -p memcore use_provenance          # EXPECTED GREEN (all)
//!     cargo nextest run -p memcore exposure_alone_must_not_change_rank \
//!         --run-ignored only          # EXPECTED RED — see that test's doc
//!
//! Note the second and third commands must be run separately: an unfiltered
//! `--run-ignored only` name filter matches both tests by prefix.
//!
//! The `use_provenance` filter above covers commit 1's pair-half plus commit
//! 2's per-lever tests at the bottom of this file; the overlooked-bonus lever
//! lives in `scorer/tests.rs` and is caught by the same filter.

use super::*;

// Explicit, mirroring `decay_policy.rs`'s import style. `quality_multiplier`
// arrives through the `use super::*` chain (it is `pub(super)` in
// `search::filtering`, re-exported into `search` under `#[cfg(test)]`), the
// same way `search/tests/noise.rs` reaches it.
use crate::scorer::HybridWeights;
use crate::types::SearchResult;

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

/// Three whitespace-separated tokens. Whitespace is load-bearing:
/// `is_id_like_exact_query` returns false for any query containing whitespace
/// (`scorer/text.rs:130`), which pins the generic precision multiplier at 1.0.
const QUERY: &str = "exposure ranking probe";

/// Every seed carries all three query tokens in `keywords` as well as in
/// `text`, so the symbolic channel saturates uniformly (see the trap-3 note in
/// the module doc).
const SEED_KEYWORDS: &[&str] = &["exposure", "ranking", "probe"];

/// Uniform across seeds so the decay importance floor (`entry.importance * 0.3`,
/// `scorer.rs:152`) is the same number for every candidate — that shared floor
/// is what [`floored_subject_behind_a_floored_peer`] selects on.
const SEED_IMPORTANCE: f64 = 0.7;

/// `RecallConfig::default().rrf_k` (`recall_config.rs:20`). Named here because
/// it is the denominator of the decay re-injection at `scorer.rs:517` and so
/// appears in every closed-form identity below.
const RRF_K: f64 = 20.0;

/// Tolerance for "this number did not move". Sized for wall-clock drift between
/// two `hybrid_search` calls in one test: `d(recency)/dt` is
/// `0.693 / half_life_days` per day, i.e. ~2.7e-10 per millisecond relative, and
/// the decay term reaches the final score scaled by `weights.decay / rrf_k`
/// (<= 0.01). Even a one-second gap moves a final score by <1e-9.
const CLOCK_TOLERANCE: f64 = 1e-6;

/// Tolerance for the closed-form fusion identities. These are exact up to f64
/// rounding — nothing in them re-reads the clock, because both sides are built
/// from decay values *observed in the same ranked result*.
const IDENTITY_TOLERANCE: f64 = 1e-9;

/// `top_k` for [`exposure_alone_must_not_change_rank`]. Deliberately smaller
/// than the corpus: `record_access` only bumps the rows a search **returned**
/// (`search.rs:501`), so a `top_k` covering the whole corpus would bump every
/// candidate uniformly and hide the reshuffle.
const EXPOSED_TOP_K: usize = 5;

/// The 90-day seed, and the subject of [`measure_l3_tier_ratchet`]. Chosen
/// because 90 days is the one age in this corpus that is **floored** under the
/// raw half-life (`exp(-0.693*90/30) = 0.125 < 0.21`) and **not** floored under
/// the consolidated half-life (`exp(-0.693*90/60) = 0.354 > 0.21`), so the
/// half-life half of L3 is observable rather than absorbed by the floor.
const L3_SUBJECT: &str = "exposure-mid-a";

/// Seeds used by every gate/measurement here.
///
/// `days_ago` straddles the importance floor deliberately. At
/// `importance = 0.7` the floor is `0.21`, and `exp(-0.693 * d / 30) < 0.21`
/// for `d > ~68` days, so:
///   * `{0, 3, 7}` are **unfloored** — decay tracks age;
///   * `{90, 120, 150, 240, 300}` are **floored** — decay is pinned at 0.21.
///
/// Being floored is what makes the `log10(1 + access_count)` frequency term
/// (`scorer.rs:151`) inert for a candidate, which is the precondition
/// [`measure_l2_access_feedback_multiplier`] needs to attribute its whole delta
/// to `apply_access_feedback` rather than to the decay channel.
///
/// **Five floored against three unfloored is a deliberate ratio, not an
/// accident.** [`floored_subject_behind_a_floored_peer`] needs two floored
/// candidates *adjacent in the ranking*; with 5 of 8 candidates floored, the
/// pigeonhole principle guarantees such a pair exists no matter how BM25 and
/// RRF order the set. A 4/4 split could in principle alternate and leave none.
///
/// The wide freshness spread is equally deliberate: it is what makes
/// [`exposure_alone_must_not_change_rank`] a real demonstration. Exposure
/// collapses every returned row to `recency = 1.0`, so a corpus whose rows all
/// start at a similar decay would be reshuffled uniformly (i.e. not at all).
/// The reshuffle only shows up when exposed rows gain *unequally*.
///
/// `text` carries a descending count of `exposure ranking probe` repetitions
/// against an ascending filler tail, so BM25 gives the seeds distinct FTS ranks.
/// The exact resulting order is *not* relied on anywhere: subjects are selected
/// by observed rank and observed decay at run time, never hard-coded.
struct Seed {
    id: &'static str,
    days_ago: i64,
    text: &'static str,
}

const SEEDS: &[Seed] = &[
    Seed {
        id: "exposure-stale-lead",
        days_ago: 240,
        text: "exposure ranking probe exposure ranking probe exposure ranking probe exposure ranking probe",
    },
    Seed {
        id: "exposure-fresh-lead",
        days_ago: 0,
        text: "exposure ranking probe exposure ranking probe exposure ranking probe exposure ranking probe alpha bravo charlie delta",
    },
    Seed {
        id: "exposure-recent-a",
        days_ago: 3,
        text: "exposure ranking probe exposure ranking probe exposure ranking probe echo foxtrot golf hotel india juliet",
    },
    Seed {
        id: "exposure-mid-a",
        days_ago: 90,
        text: "exposure ranking probe exposure ranking probe exposure ranking probe kilo lima mike november oscar papa quebec romeo sierra tango",
    },
    Seed {
        id: "exposure-mid-b",
        days_ago: 150,
        text: "exposure ranking probe exposure ranking probe uniform victor whiskey xray yankee zulu anton berta caesar dora emil friedrich",
    },
    Seed {
        id: "exposure-recent-b",
        days_ago: 7,
        text: "exposure ranking probe exposure ranking probe gustav heinrich ida julius konrad ludwig martha nordpol otto paula quelle richard samuel theodor",
    },
    Seed {
        id: "exposure-old-c",
        days_ago: 300,
        text: "exposure ranking probe ulrich viktor wilhelm xanthos ypsilon zacharias amsterdam baltimore casablanca danemark edison florida gallipoli havana italia jerusalem",
    },
    Seed {
        id: "exposure-mid-c",
        days_ago: 120,
        text: "exposure ranking probe kilogramm liverpool madagaskar ontario paris quito roma santiago tripoli uppsala valencia washington yokohama zurich ankara bangkok colombo denver ecuador",
    },
];

/// `decay_score`'s importance floor, `entry.importance * 0.3` (`scorer.rs:152`).
/// Computed from the same literals the seeds use so the comparison is exact in
/// f64 rather than relying on the decimal expansion of `0.7 * 0.3`.
fn importance_floor() -> f64 {
    SEED_IMPORTANCE * 0.3
}

/// Same shape as `golden_corpus::seed_entry` — start from the shared
/// `memory_entry` helper and override only what the fixture needs. `path`
/// (`/test`), `category` (`fact`), `tier` (`raw`), `access_count` (0) and
/// `last_access` (`None`) come from `memory_entry` unchanged; those defaults are
/// exactly what keeps `quality_multiplier` at 1.0.
fn seed_entry(s: &Seed) -> MemoryEntry {
    let mut e = memory_entry(s.id, s.text, SEED_KEYWORDS);
    e.importance = SEED_IMPORTANCE;
    e.timestamp = (Utc::now() - chrono::Duration::days(s.days_ago)).to_rfc3339();
    e
}

/// Trap-2 precondition, asserted on every fixture build rather than assumed:
/// every seed's `quality_multiplier` is exactly 1.0, so the set-relative
/// `0.85 × top` floor at `ranking.rs:317` is never consulted and no
/// counterfactual in this file can perturb another candidate through it.
fn assert_quality_boosts_are_inert() {
    for s in SEEDS {
        let entry = seed_entry(s);
        let multiplier = quality_multiplier(&entry, None);
        assert!(
            (multiplier - 1.0).abs() < f64::EPSILON,
            "seed {} has quality_multiplier {multiplier}, not 1.0 — apply_quality_boosts \
             (ranking.rs:307-330) is set-relative, so a non-unit multiplier here would make \
             every per-lever measurement in this file set-relative too. Fix the seed, do not \
             loosen the assertions.",
            s.id
        );
    }
}

fn seeded_connection() -> Connection {
    assert_quality_boosts_are_inert();
    let mut conn = setup();
    for s in SEEDS {
        insert_entry(&mut conn, seed_entry(s));
    }
    conn
}

// ---------------------------------------------------------------------------
// Path profiles
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Profile {
    /// `decay = 0.20` — `RecallConfig::default_weights`.
    Default,
    /// `decay = 0.02` — `RecallConfig::guide_weights` (`recall_config.rs:50-56`).
    Guide,
}

impl Profile {
    /// Trap 1: `RecallConfig::load` short-circuits to `Self::default()` under
    /// `cfg!(test)` (`recall_config.rs:92`), so `TACHI_RECALL_*` env vars cannot
    /// reach a test in this crate. The profile is therefore taken from an
    /// explicit `RecallConfig` and injected through `SearchOptions`.
    ///
    /// It is injected as `opts.weights`, **not** as
    /// `path_prefix: Some("/guide")`, for two reasons that would each confound a
    /// per-lever measurement:
    ///   1. `path_prefix` is a hard candidate *filter* (`ranking.rs:71-75`, plus
    ///      the FTS/vec channel predicates) — the `/test` seeds would all be
    ///      dropped.
    ///   2. `path_prefix = "/guide"` additionally switches on the guide-scoped
    ///      quality boost `×1.12` (`filtering.rs:151-156`), re-arming exactly
    ///      the set-relative step trap 2 is about.
    ///
    /// `resolve_weights` (`search.rs:127-135`) returns `opts.weights` verbatim
    /// when it differs from `HybridWeights::default()`, and falls through to
    /// `weights_for_path` otherwise — so both arms below resolve to the intended
    /// profile.
    fn weights(self) -> HybridWeights {
        let config = RecallConfig::default();
        match self {
            Profile::Default => config.weights_for_path(""),
            Profile::Guide => config.weights_for_path("/guide"),
        }
    }

    /// The `weights.decay` coefficient of `blended + weights.decay * ds / rrf_k`
    /// (`scorer.rs:517`). L1's whole leverage is proportional to this; L2's is
    /// independent of it.
    fn decay_weight(self) -> f64 {
        self.weights().decay
    }
}

fn search_options(profile: Profile, top_k: usize, record_access: bool) -> SearchOptions {
    SearchOptions {
        top_k,
        candidates_per_channel: 64,
        record_access,
        // MMR would re-order the tail by content similarity, which has nothing
        // to do with the levers under measurement.
        mmr_threshold: None,
        weights: profile.weights(),
        recall_config: Some(RecallConfig::default()),
        ..Default::default()
    }
}

/// The same options [`search_options`] builds, with a caller-supplied
/// `RecallConfig` substituted. Built *from* `search_options` rather than beside
/// it so the two arms of the RED/GREEN pair below cannot drift apart in any
/// field except the one under test.
fn search_options_with_recall_config(
    profile: Profile,
    top_k: usize,
    record_access: bool,
    recall_config: RecallConfig,
) -> SearchOptions {
    SearchOptions {
        recall_config: Some(recall_config),
        ..search_options(profile, top_k, record_access)
    }
}

/// `RecallConfig::default()` with tachi#1446's lever-1 knob on.
///
/// Trap 1 again, in its sharpest form: `TACHI_RECALL_USE_PROVENANCE_RECENCY`
/// cannot reach this crate's tests at all (`RecallConfig::load` short-circuits
/// to `Self::default()` under `cfg!(test)`, `recall_config.rs:92`), so a
/// version of this fixture that exported the env var would run the *default*
/// profile twice and report the fix green while measuring nothing. The knob is
/// therefore injected as a value through `SearchOptions.recall_config`
/// (`search.rs:83`), the same channel the profiles use.
fn use_provenance_recency_config() -> RecallConfig {
    assert!(
        !RecallConfig::default().use_provenance_recency,
        "the knob must be OFF by default — if it were not, this test and its RED sibling would \
         be running the same configuration and the pair would prove nothing"
    );
    RecallConfig {
        use_provenance_recency: true,
        ..RecallConfig::default()
    }
}

/// Rank the whole corpus (no truncation) with the access write path OFF, so a
/// measurement never perturbs the thing it is measuring.
fn rank_all(conn: &Connection, profile: Profile) -> Vec<SearchResult> {
    let opts = search_options(profile, SEEDS.len(), false);
    let ranked = hybrid_search(conn, QUERY, &opts).expect("hybrid_search");
    assert_eq!(
        ranked.len(),
        SEEDS.len(),
        "every seed must survive candidate retrieval and filtering, else a \
         'rank delta' would be confounded with a candidate-set change; got:\n{}",
        table(&ranked)
    );
    ranked
}

// ---------------------------------------------------------------------------
// Readouts
// ---------------------------------------------------------------------------

fn table(ranked: &[SearchResult]) -> String {
    ranked
        .iter()
        .enumerate()
        .map(|(idx, r)| {
            format!(
                "  {:>2}. {:<22} final={:.9} decay={:.6} tier={:<12} access_count={}",
                idx + 1,
                r.entry.id,
                r.score.final_score,
                r.score.decay,
                r.entry.tier,
                r.entry.access_count
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn rank_of(ranked: &[SearchResult], id: &str) -> usize {
    ranked
        .iter()
        .position(|r| r.entry.id == id)
        .map(|idx| idx + 1)
        .unwrap_or_else(|| panic!("{id} is absent from the ranked set:\n{}", table(ranked)))
}

fn final_of(ranked: &[SearchResult], id: &str) -> f64 {
    ranked
        .iter()
        .find(|r| r.entry.id == id)
        .unwrap_or_else(|| panic!("{id} is absent from the ranked set:\n{}", table(ranked)))
        .score
        .final_score
}

fn decay_of(ranked: &[SearchResult], id: &str) -> f64 {
    ranked
        .iter()
        .find(|r| r.entry.id == id)
        .unwrap_or_else(|| panic!("{id} is absent from the ranked set:\n{}", table(ranked)))
        .score
        .decay
}

/// True when a candidate's observed decay sits exactly on the importance floor
/// (`entry.importance * 0.3`, `scorer.rs:152`) rather than on its `recency`
/// term. Exact f64 equality is correct here: the floor is a clock-independent
/// product of two literals, computed identically in the scorer and here.
fn is_decay_floored(result: &SearchResult) -> bool {
    (result.score.decay - importance_floor()).abs() < 1e-12
}

/// Select a measurement subject: the highest-ranked candidate that is **below
/// rank 1**, is **decay-floored**, and whose **immediate predecessor is also
/// decay-floored**.
///
/// All three conditions are load-bearing, and the third is the one that took a
/// rewrite to get right.
///
/// * *Below rank 1* — a rank-1 subject can only lose positions, so a rank delta
///   would be unmeasurable.
///
/// * *On the floor* — `decay = max(recency * (1 + 0.2*log10(1+access_count)),
///   importance*0.3)` (`scorer.rs:150-154`). A candidate pinned to the floor is
///   one whose `recency × frequency` product is far below it, which makes the
///   *frequency* half inert and lets [`measure_l2_access_feedback_multiplier`]
///   attribute its whole delta to `apply_access_feedback` instead of to the
///   decay channel. It is also the maximum-leverage case for L1, since
///   `last_access = now` lifts it all the way to `recency = 1.0`.
///
/// * *Predecessor also on the floor* — this fixes what the subject's rank delta
///   actually measures. The score gap a candidate must clear to gain one
///   position has two components: the retrieval-fusion gap (`blended`, i.e. RRF
///   rank spacing, ~1e-3 here) and the decay gap. If the candidate directly
///   above were *unfloored*, its decay term alone could be up to
///   `weights.decay * (1.0 - 0.21) / rrf_k` larger — which on the default
///   profile is `7.9e-3`, the *entire* size of the L1 effect being measured. A
///   lever would then have to out-jump a decay difference rather than a rank
///   spacing, and "did this lever buy a rank position" would answer a question
///   about the fixture's freshness spread instead of about the lever. Pinning
///   both sides of the gap to the same decay floor makes the gap purely a
///   retrieval-fusion gap, so a rank movement means "this lever out-values one
///   RRF rank step" — which is exactly #1446's claim.
///
/// Selected by observation, never hard-coded, so the harness survives a BM25 or
/// RRF re-calibration that reshuffles the corpus. Existence is guaranteed by the
/// 5-floored-of-8 ratio in [`SEEDS`] (pigeonhole); if the property is gone it
/// panics with the full table rather than silently measuring something else.
fn floored_subject_behind_a_floored_peer(ranked: &[SearchResult]) -> (usize, String) {
    ranked
        .windows(2)
        .position(|pair| is_decay_floored(&pair[0]) && is_decay_floored(&pair[1]))
        .map(|idx| (idx + 2, ranked[idx + 1].entry.id.clone()))
        .unwrap_or_else(|| {
            panic!(
                "this fixture no longer ranks two decay-floored candidates adjacently, so every \
                 per-lever rank delta in this file would be measuring a decay gap instead of a \
                 retrieval-fusion gap. Restore the 5-floored-of-8 ratio in SEEDS (see its doc) \
                 rather than relaxing the selection.\n{}",
                table(ranked)
            )
        })
}

// ---------------------------------------------------------------------------
// Single-column mutations
//
// These write the `memories` columns `record_access` writes, one at a time, so
// each lever can be moved in isolation — which the production UPDATE at
// `access.rs:112-117` deliberately cannot do (it moves `access_count` and
// `last_access` in one statement; that coupling is lever 1's whole story).
//
// None of these statements writes `metadata`, so the
// `memories_reserved_refs_update_guard` trigger — `BEFORE UPDATE OF metadata`
// (`db/schema/ddl.rs:40-41`) — does not fire.
// ---------------------------------------------------------------------------

fn set_last_access(conn: &Connection, id: &str, value: &str) {
    let changed = conn
        .execute(
            "UPDATE memories SET last_access = ?1 WHERE id = ?2",
            rusqlite::params![value, id],
        )
        .expect("set last_access");
    assert_eq!(changed, 1, "expected exactly one row updated for {id}");
}

fn set_access_count(conn: &Connection, id: &str, count: i64) {
    let changed = conn
        .execute(
            "UPDATE memories SET access_count = ?1 WHERE id = ?2",
            rusqlite::params![count, id],
        )
        .expect("set access_count");
    assert_eq!(changed, 1, "expected exactly one row updated for {id}");
}

fn set_tier(conn: &Connection, id: &str, tier: &str) {
    let changed = conn
        .execute(
            "UPDATE memories SET tier = ?1 WHERE id = ?2",
            rusqlite::params![tier, id],
        )
        .expect("set tier");
    assert_eq!(changed, 1, "expected exactly one row updated for {id}");
}

fn set_promotion_counters(conn: &Connection, id: &str, recall_count: i64, query_diversity: i64) {
    let changed = conn
        .execute(
            "UPDATE memories SET recall_count = ?1, query_diversity = ?2 WHERE id = ?3",
            rusqlite::params![recall_count, query_diversity, id],
        )
        .expect("set promotion counters");
    assert_eq!(changed, 1, "expected exactly one row updated for {id}");
}

fn read_tier(conn: &Connection, id: &str) -> String {
    conn.query_row(
        "SELECT tier FROM memories WHERE id = ?1",
        rusqlite::params![id],
        |row| row.get(0),
    )
    .expect("read tier")
}

fn read_access_count(conn: &Connection, id: &str) -> i64 {
    conn.query_row(
        "SELECT access_count FROM memories WHERE id = ?1",
        rusqlite::params![id],
        |row| row.get(0),
    )
    .expect("read access_count")
}

fn read_last_access(conn: &Connection, id: &str) -> Option<String> {
    conn.query_row(
        "SELECT last_access FROM memories WHERE id = ?1",
        rusqlite::params![id],
        |row| row.get(0),
    )
    .expect("read last_access")
}

fn read_promotion_counters(conn: &Connection, id: &str) -> (i64, i64) {
    conn.query_row(
        "SELECT recall_count, query_diversity FROM memories WHERE id = ?1",
        rusqlite::params![id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .expect("read promotion counters")
}

/// Insert `count` `access_history` rows of one provenance, all aged
/// `days_ago`, without touching any `memories` column — tachi#1446 commits 2+.
///
/// Written straight to SQL rather than through `record_access_with_updates` /
/// `record_memory_use` on purpose: those two also move `access_count` /
/// `last_use_at`, and every test below needs exactly one channel to move at a
/// time. `days_ago` is the knob that decides whether the ACT-R base-level
/// activation these rows produce clears the importance floor — see
/// [`USE_EVENT_AGE_DAYS`].
fn seed_access_events(conn: &Connection, id: &str, kind: &str, count: usize, days_ago: i64) {
    let at = (Utc::now() - chrono::Duration::days(days_ago)).to_rfc3339();
    for _ in 0..count {
        conn.execute(
            "INSERT INTO access_history (memory_id, accessed_at, query_hash, event_kind)
             VALUES (?1, ?2, '', ?3)",
            rusqlite::params![id, at, kind],
        )
        .expect("seed access_history row");
    }
}

/// Age for seeded **use** events in the lever-2/4 tests, chosen so the ACT-R
/// base-level activation those rows produce stays *below* the importance
/// floor, exactly as [`measure_l2_access_feedback_multiplier`] keeps the decay
/// frequency term below it.
///
/// `base_level_activation` sums `(age_days)^-d` with `d = 0.5` for a `raw`
/// tier, and the normalisation is `(ln(sum) + 5) / 10`. At 3000 days two rows
/// give `2 * 3000^-0.5 = 0.0365`, `ln = -3.31`, normalised `0.169` — under the
/// `0.7 * 0.3 = 0.21` floor, so the floor still wins and the decay channel
/// does not move. Fresh use events would lift decay to ~0.73 and the
/// multiplier assertion would then be measuring two levers at once.
const USE_EVENT_AGE_DAYS: i64 = 3000;

/// [`rank_all`] with an explicit [`RecallConfig`] — the knob-on arm.
fn rank_all_with_config(conn: &Connection, profile: Profile, config: RecallConfig) -> Vec<SearchResult> {
    let opts = search_options_with_recall_config(profile, SEEDS.len(), false, config);
    let ranked = hybrid_search(conn, QUERY, &opts).expect("hybrid_search");
    assert_eq!(
        ranked.len(),
        SEEDS.len(),
        "every seed must survive candidate retrieval and filtering, else a \
         'rank delta' would be confounded with a candidate-set change; got:\n{}",
        table(&ranked)
    );
    ranked
}

/// The freshest candidate whose decay is **not** pinned to the importance
/// floor, i.e. the one candidate in this corpus on which the decay frequency
/// term `log10(1 + n)` is observable rather than absorbed.
///
/// Selected by observation for the same reason
/// [`floored_subject_behind_a_floored_peer`] is: a BM25/RRF recalibration must
/// change which seed this is, not whether the test is measuring the right
/// thing.
fn unfloored_subject(ranked: &[SearchResult]) -> String {
    ranked
        .iter()
        .find(|r| !is_decay_floored(r))
        .map(|r| r.entry.id.clone())
        .unwrap_or_else(|| {
            panic!(
                "this fixture no longer ranks any unfloored candidate, so the decay frequency \
                 term is absorbed by the importance floor for every seed and lever 2 cannot be \
                 measured at all. Restore the fresh end of SEEDS.\n{}",
                table(ranked)
            )
        })
}

// ---------------------------------------------------------------------------
// L1 — the `last_access` reset
// ---------------------------------------------------------------------------

/// MEASUREMENT — lever 1, predicted dominant.
///
/// Mutates **only** `last_access` (to "now") on one candidate at rank > 1,
/// leaving `access_count`, `tier`, content, query and the wall clock alone, and
/// records what that is worth in score and in rank positions, on both path
/// profiles.
///
/// Committed magnitudes, all hand-derived from source and then asserted against
/// the corpus:
///   * baseline decay `== importance * 0.3 == 0.21` (`scorer.rs:152`);
///   * post-mutation decay `== 1.0` — `age_days ≈ 0` ⇒
///     `recency = exp(-0.693 * 0 / 30) = 1.0` and `frequency = log10(1+0) = 0`
///     (`scorer.rs:150-154`), so `Δdecay == 0.79`;
///   * `Δfinal == weights.decay * Δdecay / rrf_k` exactly — the fusion identity
///     at `scorer.rs:517`, which holds only because the multiplicative envelope
///     is 1.0 (see the module doc). At `decay = 0.20, rrf_k = 20` that is
///     `0.20 * 0.79 / 20 == 7.9e-3`;
///   * the same mutation on `/guide` (`decay = 0.02`) is worth exactly `10×`
///     less — `0.20 / 0.02`. This is the whole reason L1 is called
///     decay-weight-scaled and L2 is not.
///
/// The rank claim is corpus-relative and therefore a real measurement rather
/// than arithmetic: `Δfinal` is compared against the *observed* score gap the
/// candidate has to clear to gain a single position.
#[test]
fn measure_l1_last_access_reset() {
    let conn = seeded_connection();

    let base_default = rank_all(&conn, Profile::Default);
    let base_guide = rank_all(&conn, Profile::Guide);

    let (base_rank, subject) = floored_subject_behind_a_floored_peer(&base_default);
    assert!(
        base_rank > 1,
        "L1 needs a subject below rank 1 to have room to climb; got rank {base_rank}\n{}",
        table(&base_default)
    );

    let before_final = final_of(&base_default, &subject);
    let before_decay = decay_of(&base_default, &subject);
    assert!(
        (before_decay - importance_floor()).abs() < 1e-12,
        "subject {subject} must start pinned on the importance floor {} (scorer.rs:152); got {before_decay}",
        importance_floor()
    );
    assert!(
        (decay_of(&base_guide, &subject) - before_decay).abs() < CLOCK_TOLERANCE,
        "decay is a property of the entry, not of the weights — the two profiles must agree \
         before any mutation"
    );

    // The observed score gap between the subject and the candidate immediately
    // above it: the price of exactly one rank position, in this corpus, today.
    // Both sides are decay-floored (that is what the selector guarantees), so
    // this is a pure retrieval-fusion gap — an RRF rank step — and not a
    // freshness difference in disguise.
    let predecessor = &base_default[base_rank - 2];
    assert!(
        is_decay_floored(predecessor),
        "selector invariant broken: {} sits above the subject but is not decay-floored",
        predecessor.entry.id
    );
    let gap_above = predecessor.score.final_score - before_final;
    assert!(
        gap_above > 0.0,
        "ranked order must be strictly descending here"
    );

    // ---- the counterfactual: last_access only -----------------------------
    let access_count_before = read_access_count(&conn, &subject);
    let tier_before = read_tier(&conn, &subject);
    set_last_access(&conn, &subject, &Utc::now().to_rfc3339());
    assert_eq!(
        read_access_count(&conn, &subject),
        access_count_before,
        "L1 must move last_access ONLY — access_count moved, so this would be measuring L1+L2"
    );
    assert_eq!(
        read_tier(&conn, &subject),
        tier_before,
        "L1 must move last_access ONLY — tier moved, so this would be measuring L1+L3"
    );

    let after_default = rank_all(&conn, Profile::Default);
    let after_guide = rank_all(&conn, Profile::Guide);

    let after_rank = rank_of(&after_default, &subject);
    let after_final = final_of(&after_default, &subject);
    let after_decay = decay_of(&after_default, &subject);

    // ---- committed magnitudes ---------------------------------------------
    assert!(
        (after_decay - 1.0).abs() < 1e-6,
        "one exposure sets age_days to ~0, so recency (scorer.rs:150) must be 1.0 and the \
         frequency term log10(1+0) must be 0 — expected decay 1.0, got {after_decay}"
    );
    let delta_decay = after_decay - before_decay;
    assert!(
        (delta_decay - 0.79).abs() < 1e-5,
        "hand-derived in #1446: 1.0 - (0.7 * 0.3) = 0.79; got {delta_decay}"
    );

    let delta_default = after_final - before_final;
    let identity_default = Profile::Default.decay_weight() * delta_decay / RRF_K;
    assert!(
        (delta_default - identity_default).abs() < IDENTITY_TOLERANCE,
        "L1 must reach the final score ONLY through `blended + weights.decay * ds / rrf_k` \
         (scorer.rs:517). Observed Δfinal {delta_default} != identity {identity_default}; a \
         mismatch means some multiplicative boost step is no longer 1.0 for this fixture and \
         every per-lever number in this file is now set-relative.\n{}",
        table(&after_default)
    );
    assert!(
        (delta_default - 7.9e-3).abs() < 1e-5,
        "#1446 hand-derived 0.20 * 0.79 / 20 = 7.9e-3 per exposure on the default profile; \
         measured {delta_default}"
    );

    // ---- what that is worth in rank positions ------------------------------
    assert!(
        delta_default > gap_above,
        "one exposure ({delta_default}) must out-value the price of one rank position in this \
         corpus ({gap_above}) — this is #1446's 'one exposure is worth several rank positions' \
         claim, measured\n{}",
        table(&after_default)
    );
    assert!(
        after_rank < base_rank,
        "{subject} must climb: rank {base_rank} -> {after_rank}\n{}",
        table(&after_default)
    );

    // ---- nothing else moved ------------------------------------------------
    for r in &base_default {
        if r.entry.id == subject {
            continue;
        }
        let moved = (final_of(&after_default, &r.entry.id) - r.score.final_score).abs();
        assert!(
            moved < CLOCK_TOLERANCE,
            "{} moved by {moved} without being mutated — a per-lever measurement requires the \
             rest of the candidate set to be inert (see trap 2 in the module doc)",
            r.entry.id
        );
    }

    // ---- the decay-weight contrast ----------------------------------------
    let delta_guide = final_of(&after_guide, &subject) - final_of(&base_guide, &subject);
    let identity_guide = Profile::Guide.decay_weight() * delta_decay / RRF_K;
    assert!(
        (delta_guide - identity_guide).abs() < IDENTITY_TOLERANCE,
        "same fusion identity on /guide: expected {identity_guide}, got {delta_guide}"
    );
    assert!(
        (delta_default / delta_guide - 10.0).abs() < 1e-6,
        "L1's leverage is proportional to weights.decay, so the identical mutation must be \
         worth exactly 0.20/0.02 = 10x more on the default profile than on /guide; measured \
         {}",
        delta_default / delta_guide
    );

    let default_gain = base_rank as i64 - after_rank as i64;
    let guide_gain = rank_of(&base_guide, &subject) as i64 - rank_of(&after_guide, &subject) as i64;
    assert!(
        guide_gain <= default_gain,
        "L1 cannot buy more rank at decay=0.02 ({guide_gain}) than at decay=0.20 \
         ({default_gain})\nguide after:\n{}",
        table(&after_guide)
    );
}

// ---------------------------------------------------------------------------
// L2 — the access-feedback multiplier
// ---------------------------------------------------------------------------

/// MEASUREMENT — lever 2, and the discriminating experiment.
///
/// Mutates **only** `access_count` (0, 2, 10, 100) with `last_access` held at
/// `NULL`, on both path profiles, and records score and rank deltas.
///
/// Running both profiles is the point, not a formality. L1 reaches the score
/// through `weights.decay * ds / rrf_k`, so on `/guide` (`decay = 0.02`) its
/// leverage is cut 10× — while `apply_access_feedback` multiplies the *final*
/// score (`ranking.rs:612-616`) and is untouched by the weights. If L2 is real
/// it therefore still moves rank at `decay = 0.02` where L1 barely can, and this
/// test asserts exactly that comparison on the same candidate in the same
/// fixture.
///
/// One thing #1446's lever table understates and this test pins down:
/// `access_count` is **also** the frequency term of the decay formula
/// (`let frequency = (1.0 + entry.access_count as f64).log10();`,
/// `scorer.rs:151`), so mutating it is not a pure multiplier probe in general.
/// Here it is, because the subject is selected on the importance floor: the
/// floor absorbs the frequency term entirely. Worst case in this corpus is the
/// youngest floored seed at 90 days — `recency = exp(-0.693*90/30) = 0.1251`,
/// and even at `n = 100` that is only `0.1251 * (1 + 0.2*log10(101)) = 0.1752`,
/// still under the `0.21` floor — so the decay channel does not move for any
/// probed `n`. That is asserted per-`n`, not assumed — if it ever
/// stops holding, the exact-multiplier assertion is no longer attributable to
/// `apply_access_feedback` and this test says so instead of quietly averaging
/// two levers together.
///
/// Committed multipliers, `min(1 + ln(1+n) * 0.03, 1.25)`:
/// `n=0 → 1.0` (below the `>= 2` gate), `n=2 → 1.03295837`,
/// `n=10 → 1.07193686`, `n=100 → 1.13845362`. None reaches the 1.25 clamp.
#[test]
fn measure_l2_access_feedback_multiplier() {
    for profile in [Profile::Default, Profile::Guide] {
        let conn = seeded_connection();
        let base = rank_all(&conn, profile);
        let (base_rank, subject) = floored_subject_behind_a_floored_peer(&base);
        let base_final = final_of(&base, &subject);
        let base_decay = decay_of(&base, &subject);
        assert!(
            (base_decay - importance_floor()).abs() < 1e-12,
            "{profile:?}: subject {subject} must start on the importance floor so the \
             log10(1+access_count) frequency term (scorer.rs:151) is inert; got {base_decay}"
        );

        // (n, rank, final_score) for every probed access_count.
        let mut observed: Vec<(i64, usize, f64)> = Vec::new();

        for n in [0_i64, 2, 10, 100] {
            set_access_count(&conn, &subject, n);
            assert!(
                read_last_access(&conn, &subject).is_none(),
                "L2 must move access_count ONLY — last_access is set, so this would be \
                 measuring L1+L2"
            );

            let ranked = rank_all(&conn, profile);
            let observed_final = final_of(&ranked, &subject);
            let observed_decay = decay_of(&ranked, &subject);

            assert!(
                (observed_decay - base_decay).abs() < 1e-12,
                "{profile:?} n={n}: the decay channel moved ({base_decay} -> {observed_decay}). \
                 access_count feeds BOTH apply_access_feedback and the decay frequency term \
                 (scorer.rs:151); this test is only a clean L2 measurement while the importance \
                 floor absorbs the latter."
            );

            let expected_multiplier = if n >= 2 {
                (1.0 + (n as f64).ln_1p() * 0.03).min(1.25)
            } else {
                1.0
            };
            let observed_multiplier = observed_final / base_final;
            assert!(
                (observed_multiplier - expected_multiplier).abs() < IDENTITY_TOLERANCE,
                "{profile:?} n={n}: apply_access_feedback (ranking.rs:612-616) must multiply the \
                 final score by exactly min(1 + ln1p(n)*0.03, 1.25) = {expected_multiplier}; \
                 measured {observed_multiplier}\n{}",
                table(&ranked)
            );

            observed.push((n, rank_of(&ranked, &subject), observed_final));
        }

        // Committed magnitudes (recomputed above from the same closed form, so
        // these pin the decimal expansion rather than restating the formula).
        let multiplier_at = |n: i64| -> f64 {
            observed
                .iter()
                .find(|(probe, _, _)| *probe == n)
                .map(|(_, _, score)| score / base_final)
                .expect("probed n")
        };
        assert!((multiplier_at(2) - 1.032_958_37).abs() < 1e-6);
        assert!((multiplier_at(10) - 1.071_936_86).abs() < 1e-6);
        assert!((multiplier_at(100) - 1.138_453_62).abs() < 1e-6);

        // Rank must never get worse as exposure accumulates, and must strictly
        // improve by n=100 — on BOTH profiles. That "both" is the load-bearing
        // half: it is what distinguishes L2 from L1.
        for window in observed.windows(2) {
            let (previous_n, previous_rank, _) = window[0];
            let (next_n, next_rank, _) = window[1];
            assert!(
                next_rank <= previous_rank,
                "{profile:?}: rank must be monotone in access_count; n={previous_n} gave rank \
                 {previous_rank} but n={next_n} gave {next_rank}"
            );
        }
        let (_, rank_at_zero, _) = observed[0];
        let (_, rank_at_hundred, final_at_hundred) = observed[observed.len() - 1];
        assert!(
            rank_at_hundred < rank_at_zero,
            "{profile:?}: L2 must buy rank even at decay={} — it multiplies the final score and \
             is independent of the decay weight. rank {rank_at_zero} -> {rank_at_hundred}",
            profile.decay_weight()
        );
        assert!(
            base_rank == rank_at_zero,
            "n=0 must reproduce the untouched baseline rank {base_rank}, got {rank_at_zero}"
        );

        // ---- the discriminating comparison, on /guide only -----------------
        // Same candidate, same fixture, decay weight 0.02: how much rank does
        // one exposure buy through L1 (the last_access reset) versus through L2
        // (the accumulated counter)? #1446 predicts L1 is throttled here and L2
        // is not.
        if profile == Profile::Guide {
            set_access_count(&conn, &subject, 0);
            set_last_access(&conn, &subject, &Utc::now().to_rfc3339());
            let l1_ranked = rank_all(&conn, profile);
            let l1_delta = final_of(&l1_ranked, &subject) - base_final;
            let l2_delta = final_at_hundred - base_final;
            assert!(
                l2_delta > 3.0 * l1_delta,
                "at decay=0.02 the access-feedback multiplier must out-move the last_access \
                 reset by a wide margin: L1 Δ={l1_delta} (capped at 0.02*0.79/20 = 7.9e-4), \
                 L2 Δ={l2_delta}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// L3 — the durable tier ratchet
// ---------------------------------------------------------------------------

/// MEASUREMENT — lever 3, the one irreversible lever.
///
/// Two halves:
///
/// **The gate.** Drives `recall_count` / `query_diversity` across
/// `access.rs:196-200` through the *real* `record_access_with_updates` write
/// path, with one candidate one bump short on each side of the `AND`, so both
/// halves of the conjunction are pinned rather than just the promotion itself.
///
/// **The delta.** On a clean fixture, flips `tier` alone (nothing else) on a
/// 90-day candidate and measures what promotion is worth. 90 days is chosen so
/// both halves of L3 are visible: floored under the raw half-life
/// (`exp(-0.693*90/30) = 0.125 < 0.21`) and unfloored under the consolidated one
/// (`exp(-0.693*90/60) = 0.354`), so promotion moves the decay channel *and*
/// applies the `("consolidated", false) => 1.08` boost (`ranking.rs:634`). The
/// two are separated by an exact identity rather than reported as one lump.
#[test]
fn measure_l3_tier_ratchet() {
    // ---- half 1: the promotion gate itself ---------------------------------
    {
        let conn = seeded_connection();

        // One bump short on recall_count (1 -> 2, needs >= 3), diversity fine.
        set_promotion_counters(&conn, "exposure-mid-b", 1, 3);
        // One bump short on query_diversity (1 -> 2, needs >= 3), recall fine.
        set_promotion_counters(&conn, "exposure-old-c", 2, 1);
        // One bump short of BOTH thresholds, i.e. this call crosses the gate.
        set_promotion_counters(&conn, L3_SUBJECT, 2, 3);

        let ids = vec![
            L3_SUBJECT.to_string(),
            "exposure-mid-b".to_string(),
            "exposure-old-c".to_string(),
        ];
        // Passing the same ids as `fts_hits` is what production does for any
        // FTS-matched row (`search.rs:502`), and is what increments
        // `recall_count` (`access.rs:144-153`). A fresh query string means each
        // id sees this hash for the first time, which is the
        // `HAVING COUNT(*) = 1` condition that increments `query_diversity`
        // (`access.rs:156-190`).
        record_access_with_updates(&conn, &ids, &ids, Some("l3 promotion gate probe"))
            .expect("record_access_with_updates");

        assert_eq!(
            read_promotion_counters(&conn, L3_SUBJECT),
            (3, 4),
            "one access must take {L3_SUBJECT} from (2, 3) to (3, 4)"
        );
        assert_eq!(
            read_tier(&conn, L3_SUBJECT),
            "consolidated",
            "recall_count 3 >= 3 AND query_diversity 4 >= 3 must promote (access.rs:196-200)"
        );
        assert_eq!(
            read_tier(&conn, "exposure-mid-b"),
            "raw",
            "recall_count 2 < 3 must NOT promote, even with query_diversity 4 >= 3"
        );
        assert_eq!(
            read_tier(&conn, "exposure-old-c"),
            "raw",
            "query_diversity 2 < 3 must NOT promote, even with recall_count 3 >= 3"
        );
    }

    // ---- half 2: what the tier change is worth -----------------------------
    let conn = seeded_connection();
    let base = rank_all(&conn, Profile::Default);
    let base_rank = rank_of(&base, L3_SUBJECT);
    assert!(
        base_rank > 1,
        "L3's subject needs room to climb; {L3_SUBJECT} is already rank 1\n{}",
        table(&base)
    );
    let base_final = final_of(&base, L3_SUBJECT);
    let base_decay = decay_of(&base, L3_SUBJECT);
    assert!(
        (base_decay - importance_floor()).abs() < 1e-12,
        "{L3_SUBJECT} must start floored under the 30-day raw half-life; got {base_decay}"
    );

    set_tier(&conn, L3_SUBJECT, "consolidated");
    assert_eq!(
        read_access_count(&conn, L3_SUBJECT),
        0,
        "L3 must move tier ONLY — access_count moved"
    );
    assert!(
        read_last_access(&conn, L3_SUBJECT).is_none(),
        "L3 must move tier ONLY — last_access moved"
    );

    let after = rank_all(&conn, Profile::Default);
    let after_rank = rank_of(&after, L3_SUBJECT);
    let after_final = final_of(&after, L3_SUBJECT);
    let after_decay = decay_of(&after, L3_SUBJECT);

    // Half-life 30d -> 60d (recall_config.rs:11-12) lifts a 90-day candidate off
    // the importance floor: exp(-0.693 * 90 / 60) = 0.35358.
    assert!(
        (after_decay - 0.353_58).abs() < 5e-3,
        "promotion must double the half-life: expected decay ~0.35358 at 90 days under the \
         consolidated half-life, got {after_decay}"
    );
    assert!(
        after_decay > base_decay,
        "promotion must raise the decay channel, not only apply the tier boost"
    );

    // Exact decomposition. `blended` is recovered from the baseline by removing
    // the decay re-injection (`scorer.rs:517`); the promoted score must then be
    // that same `blended`, re-injected with the NEW decay, times exactly the
    // 1.08 non-wiki consolidated tier boost (`ranking.rs:634`) — and nothing
    // else. This is what separates the half-life half of L3 from the tier-boost
    // half instead of reporting one lump.
    let decay_weight = Profile::Default.decay_weight();
    let blended = base_final - decay_weight * base_decay / RRF_K;
    let expected_after = (blended + decay_weight * after_decay / RRF_K) * 1.08;
    assert!(
        (after_final - expected_after).abs() < IDENTITY_TOLERANCE,
        "L3 must reach the final score through exactly (half-life change in decay) x 1.08 tier \
         boost: expected {expected_after}, got {after_final}\n{}",
        table(&after)
    );

    assert!(
        after_rank < base_rank,
        "promotion must buy rank: {L3_SUBJECT} {base_rank} -> {after_rank}\n{}",
        table(&after)
    );

    // Nothing else moved: only the promoted row's tier changed.
    for r in &base {
        if r.entry.id == L3_SUBJECT {
            continue;
        }
        let moved = (final_of(&after, &r.entry.id) - r.score.final_score).abs();
        assert!(
            moved < CLOCK_TOLERANCE,
            "{} moved by {moved} without being mutated",
            r.entry.id
        );
    }
}

// ---------------------------------------------------------------------------
// The regression gate
// ---------------------------------------------------------------------------

/// **RED TODAY, ON PURPOSE — tachi#1446.**
///
/// Runs one query, then runs the *same* query again through the real
/// `record_access` write path (`search.rs:497-511`), and asserts the returned
/// ordering is unchanged. No content change, no query change, no clock change
/// beyond the milliseconds the first call took. The only difference between the
/// two rankings is the access record the first call wrote about itself.
///
/// It fails today because of lever 1. `record_access` bumps only the rows a
/// search **returned** (`search.rs:501`), and the bump sets `last_access = now`
/// (`access.rs:112-117`), which resets `recency` to 1.0 for those rows only.
/// Candidates are not equally affected: a fresh row was already near
/// `recency ≈ 1.0` and gains almost nothing, while a stale row pinned at the
/// importance floor gains the full `0.79 × weights.decay / rrf_k`. Exposure
/// therefore *compresses* the top-K's decay spread, and any stale-but-strongly-
/// matching row that the decay gradient had been holding down climbs past
/// fresher neighbours. That is the loop: being shown is what earns the rank.
///
/// `#[ignore]` so it does not red the suite before the fix exists. When the
/// exposure loop is cut, delete the `#[ignore]` and this becomes the permanent
/// regression gate. **Do not weaken the assertion to make it green** — a
/// top-K-membership check, a sorted-set comparison, or a tolerance would all
/// pass while the defect is fully intact. The frozen claim is byte-identical
/// ordering.
#[test]
#[ignore = "tachi#1446: RED by design — exposure currently changes rank. Un-ignore when the loop is cut; do not weaken the assertion."]
fn exposure_alone_must_not_change_rank() {
    let conn = seeded_connection();

    let first = hybrid_search(
        &conn,
        QUERY,
        &search_options(Profile::Default, EXPOSED_TOP_K, true),
    )
    .expect("first hybrid_search");
    let second = hybrid_search(
        &conn,
        QUERY,
        &search_options(Profile::Default, EXPOSED_TOP_K, true),
    )
    .expect("second hybrid_search");

    let first_order: Vec<&str> = first.iter().map(|r| r.entry.id.as_str()).collect();
    let second_order: Vec<&str> = second.iter().map(|r| r.entry.id.as_str()).collect();

    assert_eq!(
        first_order,
        second_order,
        "exposure alone changed the ranking. Nothing about the corpus, the query or the clock \
         differed between these two calls — only the access record the first call wrote about \
         its own results.\nfirst:\n{}\nsecond:\n{}",
        table(&first),
        table(&second)
    );
}

/// **GREEN — the other half of the pair, tachi#1446 commit 1.**
///
/// Byte-for-byte the same scenario as [`exposure_alone_must_not_change_rank`]
/// above: same corpus, same query, same two `record_access`-on calls, same
/// profile, same `top_k`. The *only* difference is
/// `RecallConfig::use_provenance_recency`. That test is RED at default config
/// and this one is green with the knob on, so the pair is the proof: the defect
/// is real, and this specific switch is what removes it. Neither test alone
/// says that — a lone green could be green because the fixture is weak.
///
/// The mechanism is a read swap, not a write. `default_decay_score_with_config`
/// takes the age reference from `last_use_at` instead of `last_access`, and
/// nothing in this commit writes `last_use_at`, so every row falls through to
/// its content `timestamp` on both calls — which is exactly the state an
/// unexposed row is in. The final assertion pins that: if the column were
/// silently being written, this test would still be green for the wrong reason.
///
/// **What this test claims, and what its siblings claim.** `access_count` is
/// still bumped by exposure; what the knob changes is who reads it. Commit 1
/// switched only the age reference, so this test's frozen claim is narrow: the
/// ordering does not move, and the decay channel keeps a range instead of
/// collapsing onto one value. The remaining read points — the decay frequency
/// term, the access-feedback multiplier, the ACT-R base-level-activation floor
/// and the overlooked bonus — get one test each in the section at the bottom of
/// this file (and, for the overlooked bonus, in `scorer/tests.rs`), because
/// each has to be shown *switched* rather than deleted, which needs a knob-off
/// control this test does not carry.
#[test]
fn use_provenance_recency_keeps_exposure_out_of_rank() {
    let conn = seeded_connection();
    let config = use_provenance_recency_config();

    let first = hybrid_search(
        &conn,
        QUERY,
        &search_options_with_recall_config(Profile::Default, EXPOSED_TOP_K, true, config.clone()),
    )
    .expect("first hybrid_search");
    let second = hybrid_search(
        &conn,
        QUERY,
        &search_options_with_recall_config(Profile::Default, EXPOSED_TOP_K, true, config),
    )
    .expect("second hybrid_search");

    let first_order: Vec<&str> = first.iter().map(|r| r.entry.id.as_str()).collect();
    let second_order: Vec<&str> = second.iter().map(|r| r.entry.id.as_str()).collect();

    assert_eq!(
        first_order,
        second_order,
        "with use_provenance_recency ON, exposure must not be able to reach the ranking at \
         all — the decay channel's age reference is a column the search path never \
         writes.\nfirst:\n{}\nsecond:\n{}",
        table(&first),
        table(&second)
    );

    // The defect measured in commit 0 is not inflation, it is *range collapse*:
    // after one exposure every returned row landed on the same decay value, so
    // a channel carrying 20% of the default profile's weight stopped telling
    // candidates apart. Ordering alone would not catch a regression that
    // re-collapsed the range while happening to preserve this corpus's order,
    // so assert the range directly.
    let decays: Vec<f64> = second.iter().map(|r| r.score.decay).collect();
    let spread = decays.iter().cloned().fold(f64::MIN, f64::max)
        - decays.iter().cloned().fold(f64::MAX, f64::min);
    assert!(
        spread > 0.1,
        "the decay channel collapsed to a near-constant on the second call (spread {spread}) — \
         the fresh and stale ends of the returned set are eight raw half-lives apart, so a live \
         recency signal cannot put them within 0.1 of each other:\n{}",
        table(&second)
    );

    // Falsifier for the green above: it must come from the read swap, not from
    // some other path having quietly started writing the new column. The write
    // path that now exists (`db::record_memory_use`) is reachable only from a
    // caller-initiated save, and nothing here saves — so a non-NULL value in a
    // search-only test still means this test is green for a reason it does not
    // state.
    for r in second.iter().chain(first.iter()) {
        assert!(
            r.entry.last_use_at.is_none(),
            "{} carries last_use_at={:?}; no search path may write that column, so a non-NULL \
             value here means this test is passing for a reason it does not state",
            r.entry.id,
            r.entry.last_use_at
        );
    }
}

// ---------------------------------------------------------------------------
// The remaining levers, one test each — tachi#1446 commit 2
//
// Every test in this section has the same shape, and the shape is the point:
//   * knob ON, mutate the exposure-side channel  => the score must NOT move;
//   * knob OFF, the same mutation                => the score MUST move.
// The second half is not a formality. A lever that was accidentally deleted
// rather than switched would pass the first assertion, and only the knob-off
// control tells the two apart.
// ---------------------------------------------------------------------------

/// **Lever 5 — the ACT-R base-level-activation floor.**
///
/// `get_access_times` (`search/ranking.rs`'s `read_access_times`) feeds
/// `default_decay_score_actr_with_config`'s `Some(ages)` arm, whose normalised
/// BLA overrides the importance floor. Every row it read was written by the
/// search path about its own results, so being displayed five times lifted a
/// candidate's decay from the `0.21` floor to ~`0.82`.
///
/// With the knob on, that read is `get_use_access_times` and display rows are
/// invisible to it: the same five rows must move nothing.
#[test]
fn use_provenance_recency_keeps_display_events_out_of_the_actr_floor() {
    let conn = seeded_connection();
    let config = use_provenance_recency_config();

    let base_on = rank_all_with_config(&conn, Profile::Default, config.clone());
    let (_, subject) = floored_subject_behind_a_floored_peer(&base_on);
    let base_decay = decay_of(&base_on, &subject);
    assert!(
        (base_decay - importance_floor()).abs() < 1e-12,
        "the subject must start on the importance floor, else a BLA lift is not observable \
         against it; got {base_decay}"
    );

    // Five *recent* display events: BLA = ln(5 * (1/24)^-0.5) = 3.198,
    // normalised (3.198 + 5)/10 = 0.820, four times the floor.
    seed_access_events(&conn, &subject, "display", 5, 0);

    let after_on = rank_all_with_config(&conn, Profile::Default, config);
    assert!(
        (decay_of(&after_on, &subject) - base_decay).abs() < CLOCK_TOLERANCE,
        "with use_provenance_recency ON the ACT-R floor must not see display events: decay \
         moved {} -> {}\n{}",
        base_decay,
        decay_of(&after_on, &subject),
        table(&after_on)
    );

    // Control: the very same rows, read at default config, are exactly the
    // defect — so this fixture is not merely inert.
    let after_off = rank_all(&conn, Profile::Default);
    let off_decay = decay_of(&after_off, &subject);
    assert!(
        off_decay > base_decay + 0.5,
        "control failed: at default config five display events must lift the ACT-R floor far \
         above the importance floor ({base_decay} -> {off_decay}). If this stops holding the \
         knob-on assertion above proves nothing.\n{}",
        table(&after_off)
    );
}

/// **Lever 2 — the decay frequency term.**
///
/// `let frequency = (1.0 + entry.access_count as f64).log10();` multiplies
/// `recency` by `1 + 0.2 * frequency`, and `access_count` is incremented for
/// every row a search returns. Measured on the one candidate class where the
/// term is observable at all: an *unfloored* one (a floored candidate's
/// frequency term is absorbed by `importance * 0.3`, which is exactly why
/// [`measure_l2_access_feedback_multiplier`] selects a floored subject).
///
/// With the knob on the count comes from `event_kind = 'use'` rows, of which
/// this fixture has none, so `access_count = 100` must move nothing.
#[test]
fn use_provenance_recency_moves_the_decay_frequency_term_off_exposure() {
    let conn = seeded_connection();
    let config = use_provenance_recency_config();

    let base_on = rank_all_with_config(&conn, Profile::Default, config.clone());
    let subject = unfloored_subject(&base_on);
    let base_decay = decay_of(&base_on, &subject);

    // An untouched fixture must score identically under both arms: with no
    // `last_use_at` and no `last_access`, both recency anchors fall through to
    // the same content `timestamp`. This is the knob's byte-identity property
    // stated as an assertion rather than as a claim in a comment.
    let base_off = rank_all(&conn, Profile::Default);
    assert!(
        (decay_of(&base_off, &subject) - base_decay).abs() < CLOCK_TOLERANCE,
        "on an untouched corpus both knob arms must produce the same decay — off {}, on {}",
        decay_of(&base_off, &subject),
        base_decay
    );

    set_access_count(&conn, &subject, 100);
    assert!(
        read_last_access(&conn, &subject).is_none(),
        "this test must move access_count ONLY, or it is measuring lever 1 as well"
    );

    let after_on = rank_all_with_config(&conn, Profile::Default, config);
    assert!(
        (decay_of(&after_on, &subject) - base_decay).abs() < CLOCK_TOLERANCE,
        "with use_provenance_recency ON, access_count must not reach the decay frequency term: \
         {base_decay} -> {}\n{}",
        decay_of(&after_on, &subject),
        table(&after_on)
    );

    // Control: at default config the same counter multiplies decay by exactly
    // `1 + 0.2*log10(101) = 1.400862`.
    let after_off = rank_all(&conn, Profile::Default);
    let observed_ratio = decay_of(&after_off, &subject) / base_decay;
    let expected_ratio = 1.0 + 0.2 * 101.0_f64.log10();
    assert!(
        (observed_ratio - expected_ratio).abs() < 1e-6,
        "control failed: at default config access_count=100 must multiply this unfloored \
         candidate's decay by exactly {expected_ratio}, measured {observed_ratio}\n{}",
        table(&after_off)
    );
}

/// **Lever 4 — the access-feedback multiplier.**
///
/// `apply_access_feedback` multiplies the *final* score, so unlike levers 1-3
/// it is not scaled by `weights.decay` — on a `/guide` profile it is the
/// dominant exposure channel, which is why this test runs both profiles.
///
/// Two halves:
/// 1. knob on + `access_count = 100` (exposure) => the multiplier must not fire;
/// 2. knob on + two real `use` events           => it must fire again, with
///    exactly the multiplier [`measure_l2_access_feedback_multiplier`]
///    committed for `n = 2`, `1.03295837`.
///
/// The second half is what makes this a *switch* rather than a deletion, and
/// it is why the use events are aged [`USE_EVENT_AGE_DAYS`]: recent ones would
/// lift the ACT-R floor and the measured ratio would be two levers, not one.
#[test]
fn use_provenance_recency_moves_the_access_feedback_multiplier_off_exposure() {
    for profile in [Profile::Default, Profile::Guide] {
        let conn = seeded_connection();
        let config = use_provenance_recency_config();

        let base = rank_all_with_config(&conn, profile, config.clone());
        let (_, subject) = floored_subject_behind_a_floored_peer(&base);
        let base_final = final_of(&base, &subject);
        let base_decay = decay_of(&base, &subject);
        assert!(
            (base_decay - importance_floor()).abs() < 1e-12,
            "{profile:?}: the subject must sit on the importance floor so the decay channel is \
             inert and the whole ratio below is attributable to apply_access_feedback; got \
             {base_decay}"
        );

        set_access_count(&conn, &subject, 100);
        let exposed = rank_all_with_config(&conn, profile, config.clone());
        assert!(
            (final_of(&exposed, &subject) / base_final - 1.0).abs() < CLOCK_TOLERANCE,
            "{profile:?}: with use_provenance_recency ON, access_count must not reach \
             apply_access_feedback — final score moved {base_final} -> {}\n{}",
            final_of(&exposed, &subject),
            table(&exposed)
        );

        // Half 2: two genuine use events, old enough to leave the decay
        // channel on the floor.
        seed_access_events(&conn, &subject, "use", 2, USE_EVENT_AGE_DAYS);
        let used = rank_all_with_config(&conn, profile, config);
        let used_decay = decay_of(&used, &subject);
        assert!(
            (used_decay - base_decay).abs() < CLOCK_TOLERANCE,
            "{profile:?}: {USE_EVENT_AGE_DAYS}-day-old use events must leave the decay channel \
             on the importance floor ({base_decay} -> {used_decay}), or the ratio below is \
             measuring the ACT-R floor as well as the multiplier"
        );
        let expected_multiplier = 1.0 + 2.0_f64.ln_1p() * 0.03;
        let observed_multiplier = final_of(&used, &subject) / base_final;
        assert!(
            (observed_multiplier - expected_multiplier).abs() < IDENTITY_TOLERANCE,
            "{profile:?}: two USE events must buy exactly the multiplier two exposures used to \
             buy, min(1 + ln1p(2)*0.03, 1.25) = {expected_multiplier}; measured \
             {observed_multiplier}\n{}",
            table(&used)
        );
        assert!(
            (expected_multiplier - 1.032_958_37).abs() < 1e-6,
            "the committed decimal expansion moved; this is the same number \
             measure_l2_access_feedback_multiplier pins for n=2"
        );
    }
}

/// **Knob OFF is unchanged — the redline, as an assertion.**
///
/// Everything tachi#1446's write path adds (`event_kind = 'use'` rows and a
/// non-NULL `last_use_at`) must be invisible at default config. If it were
/// not, the new signal would be feeding the very channel this issue exists to
/// cut, on every deployment that never turns the knob on — and every golden in
/// the repo would move.
///
/// The mutations here are deliberately the *loudest* the write path can
/// produce: a `last_use_at` of now (maximum lever-1 leverage on a floored
/// candidate) and five recent use events (maximum ACT-R leverage).
#[test]
fn use_provenance_writes_are_invisible_at_default_config() {
    let conn = seeded_connection();

    let before = rank_all(&conn, Profile::Default);
    let (_, subject) = floored_subject_behind_a_floored_peer(&before);

    conn.execute(
        "UPDATE memories SET last_use_at = ?1 WHERE id = ?2",
        rusqlite::params![Utc::now().to_rfc3339(), subject],
    )
    .expect("set last_use_at");
    seed_access_events(&conn, &subject, "use", 5, 0);

    let after = rank_all(&conn, Profile::Default);

    assert_eq!(
        before.iter().map(|r| r.entry.id.as_str()).collect::<Vec<_>>(),
        after.iter().map(|r| r.entry.id.as_str()).collect::<Vec<_>>(),
        "default-config ordering moved after a use-provenance write\nbefore:\n{}\nafter:\n{}",
        table(&before),
        table(&after)
    );
    for r in &before {
        let moved = (final_of(&after, &r.entry.id) - r.score.final_score).abs();
        assert!(
            moved < CLOCK_TOLERANCE,
            "{} moved by {moved} at default config after a use-provenance write — the display \
             read (`get_access_times`) is letting `event_kind = 'use'` rows through, or a \
             default-config lever is reading `last_use_at`",
            r.entry.id
        );
    }
    assert!(
        (decay_of(&after, &subject) - decay_of(&before, &subject)).abs() < CLOCK_TOLERANCE,
        "the subject's decay channel moved at default config"
    );
}

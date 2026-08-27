//! The CurrentTruth reduction law (#1696 / #1297 §1.2).
//!
//! The reducer is a **pure, deterministic function** from the set of stored
//! assertions to per-`(subject, predicate)` reduction statuses. It performs
//! no I/O, reads no clock, and calls no model. It never rewrites history:
//! supersession is an output classification, not a mutation.
//!
//! # Law
//!
//! 1. Only assertions whose `review_state` is admitted (`Observed` /
//!    `Reviewed`) and whose `authority_class` may establish the predicate
//!    participate. Model prose and rejected evidence never affect current
//!    truth, even when they cite evidence.
//! 2. Assertions may supersede each other **only within the same lineage** —
//!    same `(subject, predicate, authority_class, issuer, source)`. Within a
//!    lineage the newest assertion by `(observed_at, source_revision,
//!    assertion_id)` is the lineage head; every older assertion is
//!    `Superseded`. An explicit `supersedes_assertion_id` edge that
//!    contradicts this order (claims to supersede a strictly newer
//!    assertion) makes the predicate `Conflicted` — malformed provenance is
//!    surfaced, never silently resolved.
//! 3. Across lineages for the same `(subject, predicate)`: lineage heads
//!    that agree on value collapse into one `Current` predicate with all
//!    heads as evidence. Lineage heads that disagree remain `Conflicted` —
//!    a newer source revision of one authority never overrides a different
//!    authority (#1696: "newer source revision may supersede an older
//!    assertion only within the same admitted predicate/authority scope").
//! 4. No admitted assertions for a `(subject, predicate)` → `Unknown`. A
//!    missing fact is never defaulted, guessed, or carried over from prose.
//! 5. Two assertions in one lineage at the same immutable source revision
//!    with **different** values are `Conflicted` (the source contradicted
//!    itself at a revision that should be immutable). The store's
//!    idempotency key rejects this at append time; the reducer classifies it
//!    deterministically anyway so a hand-built assertion set cannot smuggle
//!    it through.

use std::collections::BTreeMap;

use serde::Serialize;

use super::types::{
    AssertionV1, AssertionValueV1, EvidenceHeadV1, GithubObjectRefV1, PredicateV1,
    ReductionStatusV1, SubjectRefV1,
};

/// The reduction of one `(subject, predicate)` pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReducedPredicateV1 {
    pub subject: SubjectRefV1,
    pub predicate: PredicateV1,
    pub status: ReductionStatusV1,
    /// The current value when `Current`; empty when `Conflicted` (nothing is
    /// selected), `Superseded`, or `Unknown`.
    pub values: Vec<AssertionValueV1>,
    /// Evidence heads supporting the current (or contradicting) facts.
    /// Empty for `Superseded` and `Unknown`.
    pub current_heads: Vec<EvidenceHeadV1>,
    /// Heads that were superseded within their lineages — retained so
    /// consumers can cite replaced evidence without re-reading raw history.
    pub superseded_heads: Vec<EvidenceHeadV1>,
}

/// The full reduction output: every `(subject, predicate)` that has at least
/// one admitted assertion, in canonical (subject, predicate) order.
/// Deterministic for a given input set.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReductionV1 {
    predicates: BTreeMap<(String, PredicateV1), ReducedPredicateV1>,
}

/// Content-free reduction health — counts only, no subject tokens or refs,
/// safe for any caller (#1696 discrimination 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct ReductionHealthV1 {
    pub conflicted_predicates: usize,
    pub superseded_heads: usize,
}

impl ReductionV1 {
    /// Look up the reduction of one `(subject, predicate)`. Subjects with no
    /// admitted assertion reduce to `Unknown`.
    pub fn get(&self, subject: &SubjectRefV1, predicate: PredicateV1) -> ReducedPredicateV1 {
        self.predicates
            .get(&(subject.as_token(), predicate))
            .cloned()
            .unwrap_or_else(|| ReducedPredicateV1 {
                subject: subject.clone(),
                predicate,
                status: ReductionStatusV1::Unknown,
                values: Vec::new(),
                current_heads: Vec::new(),
                superseded_heads: Vec::new(),
            })
    }

    /// All reduced predicates in canonical order.
    pub fn all(&self) -> Vec<ReducedPredicateV1> {
        self.predicates.values().cloned().collect()
    }

    /// Subjects appearing in the reduction, in canonical token order.
    pub fn subjects(&self) -> Vec<SubjectRefV1> {
        self.predicates
            .values()
            .map(|r| r.subject.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Content-free health counts.
    pub fn health_counts(&self) -> ReductionHealthV1 {
        ReductionHealthV1 {
            conflicted_predicates: self
                .predicates
                .values()
                .filter(|r| r.status == ReductionStatusV1::Conflicted)
                .count(),
            superseded_heads: self
                .predicates
                .values()
                .map(|r| r.superseded_heads.len())
                .sum(),
        }
    }
}

/// Reduce a set of assertions. Pure and deterministic: the same set of
/// assertions — appended in any order, incrementally or in a full rebuild —
/// always yields the equal [`ReductionV1`]. Duplicate assertion ids are
/// collapsed; arrival order is never an input.
pub fn reduce(assertions: &[AssertionV1]) -> ReductionV1 {
    // Deduplicate by assertion id so a replayed read cannot double-count.
    let mut by_id: BTreeMap<&str, &AssertionV1> = BTreeMap::new();
    for assertion in assertions {
        by_id.insert(assertion.assertion_id.as_str(), assertion);
    }

    // 1. Admission gate.
    let admitted: Vec<&&AssertionV1> = by_id
        .values()
        .filter(|a| a.review_state.is_admitted())
        .filter(|a| a.authority_class.may_establish(a.predicate))
        .collect();

    // 2. Group by (subject, predicate), then by lineage.
    type LineageMap<'a> = BTreeMap<String, Vec<&'a AssertionV1>>;
    let mut grouped: BTreeMap<(String, PredicateV1), (SubjectRefV1, LineageMap<'_>)> =
        BTreeMap::new();
    let admitted: Vec<&AssertionV1> = admitted.into_iter().copied().collect();
    for assertion in &admitted {
        grouped
            .entry((assertion.subject.as_token(), assertion.predicate))
            .or_insert_with(|| (assertion.subject.clone(), BTreeMap::new()))
            .1
            .entry(lineage_bucket_key(assertion))
            .or_default()
            .push(assertion);
    }

    let mut predicates = BTreeMap::new();
    for ((subject_token, predicate), (subject, lineages)) in grouped {
        let mut current_heads = Vec::new();
        let mut superseded_heads = Vec::new();
        let mut values: Vec<AssertionValueV1> = Vec::new();
        let mut malformed = false;

        for (_, mut lineage) in lineages {
            // Deterministic order within the lineage: source-supplied only.
            lineage.sort_by_key(|assertion| assertion.order_key());
            // Explicit-edge contradiction: a `supersedes` edge pointing at a
            // strictly newer assertion is malformed provenance.
            for assertion in &lineage {
                if let Some(target) = &assertion.supersedes_assertion_id {
                    if lineage.iter().any(|other| {
                        other.assertion_id == *target && other.order_key() > assertion.order_key()
                    }) {
                        malformed = true;
                    }
                }
            }
            let Some((head, older)) = lineage.split_last() else {
                continue;
            };
            superseded_heads.extend(older.iter().map(|a| EvidenceHeadV1::of(a)));
            // Same-revision self-contradiction inside the lineage.
            for older_assertion in older {
                if older_assertion.source_ref.revision == head.source_ref.revision
                    && older_assertion.value != head.value
                {
                    malformed = true;
                }
            }
            if !values.contains(&head.value) {
                values.push(head.value.clone());
            }
            current_heads.push(EvidenceHeadV1::of(head));
        }

        let conflicted = malformed || values.len() > 1;
        sort_heads(&mut current_heads);
        sort_heads(&mut superseded_heads);
        let reduced = if conflicted {
            // Conflicted keeps every lineage head as evidence — nothing is
            // selected, nothing is dropped, nothing is guessed.
            ReducedPredicateV1 {
                subject,
                predicate,
                status: ReductionStatusV1::Conflicted,
                values: Vec::new(),
                current_heads,
                superseded_heads: Vec::new(),
            }
        } else {
            ReducedPredicateV1 {
                subject,
                predicate,
                status: ReductionStatusV1::Current,
                values,
                current_heads,
                superseded_heads,
            }
        };
        predicates.insert((subject_token, predicate), reduced);
    }

    ReductionV1 { predicates }
}

fn sort_heads(heads: &mut [EvidenceHeadV1]) {
    heads.sort_by(|a, b| {
        (
            a.observed_at.clone(),
            a.source_revision.clone(),
            a.assertion_id.clone(),
        )
            .cmp(&(
                b.observed_at.clone(),
                b.source_revision.clone(),
                b.assertion_id.clone(),
            ))
    });
}

/// Lineage bucket key: assertions supersede each other only within one
/// `(authority_class, issuer, source)` scope for the same subject+predicate.
fn lineage_bucket_key(assertion: &AssertionV1) -> String {
    format!(
        "{:?}|{}|{}",
        assertion.authority_class, assertion.issuer, assertion.source_ref.source
    )
}

/// The issue-level lifecycle view derived from reduced state predicates.
///
/// `issue_open` / `issue_closed` / `issue_reopened` are distinct predicates;
/// the *current* lifecycle is whichever currently-admitted family carries the
/// newest evidence head. `issue_reopened` counts as open with the transition
/// recorded (#1297 worked example B).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueLifecycleView {
    /// No admitted lifecycle assertion for the issue.
    Unknown,
    /// Currently open.
    Open,
    /// Currently open via an observed reopen after a close.
    Reopened,
    /// Currently closed.
    Closed,
    /// Contradictory current lifecycle facts.
    Conflicted,
}

/// The PR-level lifecycle view derived from reduced state predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrLifecycleView {
    Unknown,
    Open,
    Merged,
    ClosedUnmerged,
    /// The merge was reverted — the PR did merge, but its implementation is
    /// no longer effective.
    MergeReverted,
    Conflicted,
}

/// One resolved lifecycle family: whether the family is conflicted, and
/// which predicate owns the newest current head (`None` = no admitted
/// lifecycle fact at all).
struct ResolvedFamily {
    newest: Option<PredicateV1>,
    conflicted: bool,
}

impl ReductionV1 {
    /// The current lifecycle view of an issue subject.
    pub fn issue_lifecycle(&self, subject: &SubjectRefV1) -> IssueLifecycleView {
        let family = resolve_family(
            self,
            subject,
            &[
                PredicateV1::IssueOpen,
                PredicateV1::IssueClosed,
                PredicateV1::IssueReopened,
            ],
        );
        if family.conflicted {
            return IssueLifecycleView::Conflicted;
        }
        match family.newest {
            Some(PredicateV1::IssueClosed) => IssueLifecycleView::Closed,
            Some(PredicateV1::IssueReopened) => IssueLifecycleView::Reopened,
            Some(PredicateV1::IssueOpen) => IssueLifecycleView::Open,
            _ => IssueLifecycleView::Unknown,
        }
    }

    /// The current lifecycle view of a PR subject.
    pub fn pr_lifecycle(&self, subject: &SubjectRefV1) -> PrLifecycleView {
        let family = resolve_family(
            self,
            subject,
            &[
                PredicateV1::PrOpen,
                PredicateV1::PrMerged,
                PredicateV1::PrClosedUnmerged,
                PredicateV1::MergeReverted,
            ],
        );
        if family.conflicted {
            return PrLifecycleView::Conflicted;
        }
        match family.newest {
            Some(PredicateV1::PrOpen) => PrLifecycleView::Open,
            Some(PredicateV1::PrMerged) => PrLifecycleView::Merged,
            Some(PredicateV1::PrClosedUnmerged) => PrLifecycleView::ClosedUnmerged,
            Some(PredicateV1::MergeReverted) => PrLifecycleView::MergeReverted,
            _ => PrLifecycleView::Unknown,
        }
    }

    /// Whether implementation is effectively present for an issue: a current
    /// `implementation_present` assertion **not** negated by a newer revert
    /// on a linked PR, or a currently-linked PR whose lifecycle is `Merged`.
    /// A revert necessarily carries a newer source revision, so a reverted
    /// merge reads as not-present.
    pub fn implementation_present(&self, issue: &SubjectRefV1) -> bool {
        let explicit = self.get(issue, PredicateV1::ImplementationPresent);
        if explicit.status == ReductionStatusV1::Current {
            let newest_impl = explicit.current_heads.iter().map(evidence_head_key).max();
            let reverted_newer = self.linked_prs(issue).iter().any(|object| {
                let pr = pr_subject(issue, object);
                let revert = self.get(&pr, PredicateV1::MergeReverted);
                if revert.status != ReductionStatusV1::Current {
                    return false;
                }
                match (
                    revert.current_heads.iter().map(evidence_head_key).max(),
                    newest_impl.clone(),
                ) {
                    (Some(revert_key), Some(impl_key)) => revert_key > impl_key,
                    _ => true,
                }
            });
            if !reverted_newer {
                return true;
            }
        }
        !self.linked_merged_prs(issue).is_empty()
    }

    /// Currently-linked implementation PRs (from the current
    /// `implementation_pr_linked` value set).
    pub fn linked_prs(&self, issue: &SubjectRefV1) -> Vec<GithubObjectRefV1> {
        self.get(issue, PredicateV1::ImplementationPrLinked)
            .values
            .iter()
            .filter_map(|value| match value {
                AssertionValueV1::ObjectRef(object) => Some(object.clone()),
                _ => None,
            })
            .collect()
    }

    /// Linked PRs whose merge is currently effective (merged, not reverted).
    pub fn linked_merged_prs(&self, issue: &SubjectRefV1) -> Vec<GithubObjectRefV1> {
        self.linked_prs(issue)
            .into_iter()
            .filter(|object| {
                self.pr_lifecycle(&pr_subject(issue, object)) == PrLifecycleView::Merged
            })
            .collect()
    }

    /// Whether owner acceptance is currently present for an issue.
    pub fn owner_acceptance_present(&self, issue: &SubjectRefV1) -> bool {
        self.get(issue, PredicateV1::OwnerAcceptancePresent).status == ReductionStatusV1::Current
    }

    /// Whether any predicate in the subject's reconciliation chain (the
    /// issue plus its currently linked PRs) is conflicted.
    pub fn chain_conflicted(&self, issue: &SubjectRefV1) -> bool {
        let mut chain = vec![issue.as_token()];
        chain.extend(
            self.linked_prs(issue)
                .iter()
                .map(|object| pr_subject(issue, object).as_token()),
        );
        self.predicates.values().any(|r| {
            chain.contains(&r.subject.as_token()) && r.status == ReductionStatusV1::Conflicted
        })
    }
}

fn pr_subject(issue: &SubjectRefV1, object: &GithubObjectRefV1) -> SubjectRefV1 {
    SubjectRefV1 {
        repo: issue.repo.clone(),
        object: object.clone(),
    }
}

fn evidence_head_key(head: &EvidenceHeadV1) -> (String, String, String) {
    (
        head.observed_at.clone(),
        head.source_revision.clone(),
        head.assertion_id.clone(),
    )
}

/// Resolve a mutually-exclusive predicate family to the predicate owning
/// its newest current head, flagging conflicts (a `Conflicted` member, or
/// two members sharing the same newest head key — i.e. "open and closed at
/// the identical immutable revision").
fn resolve_family(
    reduction: &ReductionV1,
    subject: &SubjectRefV1,
    family: &[PredicateV1],
) -> ResolvedFamily {
    let mut conflicted = false;
    let mut best: Option<((String, String, String), PredicateV1)> = None;
    for predicate in family {
        let reduced = reduction.get(subject, *predicate);
        match reduced.status {
            ReductionStatusV1::Current => {
                if let Some(head) = reduced.current_heads.first() {
                    let key = (
                        head.observed_at.clone(),
                        head.source_revision.clone(),
                        head.assertion_id.clone(),
                    );
                    match &best {
                        None => best = Some((key, *predicate)),
                        Some((best_key, best_predicate)) => {
                            if key > *best_key {
                                best = Some((key, *predicate));
                            } else if key == *best_key && predicate != best_predicate {
                                conflicted = true;
                            }
                        }
                    }
                }
            }
            ReductionStatusV1::Conflicted => conflicted = true,
            ReductionStatusV1::Superseded | ReductionStatusV1::Unknown => {}
        }
    }
    ResolvedFamily {
        newest: best.map(|(_, predicate)| predicate),
        conflicted,
    }
}

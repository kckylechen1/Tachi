//! Read-only, privacy-first selection for the real #1073 pilot manifest.
//!
//! Source text exists only inside this module while rows are screened and
//! classified. Public results contain bindings and aggregate counts only.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use sha2::{Digest, Sha256};
use tachi_params::LessonCandidateKindV1;

use super::pilot::{
    freeze_pilot_manifest, PilotManifestV1, PilotRowKindV1, PilotRowV1, PilotSourceRouteV1,
    PilotStratumV1,
};
use super::privacy::{screen_manifest_metadata_for_public_pilot, screen_source_for_public_pilot};
use super::source::{
    verify_resolved_source_v1, PilotSourceResolverV1, ResolvedPilotSourceV1, SourceResolveError,
};

const SOURCE_TARGET: usize = 25;
const KIND_TARGET: usize = 25;
const STRATUM_TARGETS: [usize; 3] = [17, 17, 16];

#[derive(Debug, Clone)]
pub struct PilotSelectionConfigV1 {
    pub antigravity_db: PathBuf,
    pub hapi_db: PathBuf,
    pub capture_timestamp: String,
    pub manifest_path: PathBuf,
    pub receipt_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotSelectionReceiptV1 {
    pub contract_digest: String,
    pub rows: usize,
    pub by_source: [usize; 2],
    pub by_kind: [usize; 2],
    pub by_stratum: [usize; 3],
}

#[derive(Debug)]
pub enum PilotSelectionErrorV1 {
    OutputPathIdentity,
    OutputPathAlias,
    ReadOnlyOpen(PilotSourceRouteV1),
    ReadOnlyInvariant(PilotSourceRouteV1),
    Query(PilotSourceRouteV1),
    DuplicateBinding(PilotSourceRouteV1),
    InsufficientMatrix(Box<EligibleCounts>),
    Freeze(usize),
    Save,
    Reload,
    IndependentVerification(PilotSourceRouteV1),
    AtomicReplace(std::io::Error),
    ReceiptWrite(std::io::Error),
}

impl std::fmt::Display for PilotSelectionErrorV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutputPathIdentity => {
                write!(f, "pilot output path identity could not be resolved safely")
            }
            Self::OutputPathAlias => write!(
                f,
                "pilot manifest and receipt outputs must be distinct from both source databases and each other"
            ),
            Self::ReadOnlyOpen(route) => write!(
                f,
                "could not open the {} source in strict read-only mode",
                route.as_str()
            ),
            Self::ReadOnlyInvariant(route) => write!(
                f,
                "the {} source did not attest query_only mode",
                route.as_str()
            ),
            Self::Query(route) => write!(
                f,
                "the {} source could not be read in its stable transaction",
                route.as_str()
            ),
            Self::DuplicateBinding(route) => write!(
                f,
                "the {} eligible pool contains a duplicate route/id/revision binding",
                route.as_str()
            ),
            Self::InsufficientMatrix(counts) => write!(
                f,
                "eligible matrix shortfall: sources={}/{} kinds={}/{} strata={}/{}/{} cells={}",
                counts.by_source[0],
                counts.by_source[1],
                counts.by_kind[0],
                counts.by_kind[1],
                counts.by_stratum[0],
                counts.by_stratum[1],
                counts.by_stratum[2],
                counts
                    .cells
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Self::Freeze(count) => {
                write!(f, "selected manifest failed {count} frozen invariant(s)")
            }
            Self::Save => write!(f, "verified manifest persistence failed"),
            Self::Reload => write!(
                f,
                "independent manifest reload or digest verification failed"
            ),
            Self::IndependentVerification(route) => write!(
                f,
                "independent {} source binding verification failed",
                route.as_str()
            ),
            Self::AtomicReplace(error) => {
                write!(f, "could not atomically replace a verified pilot artifact: {error}")
            }
            Self::ReceiptWrite(error) => write!(f, "could not write public receipt: {error}"),
        }
    }
}

impl std::error::Error for PilotSelectionErrorV1 {}

#[derive(Debug, Clone)]
struct EligibleCandidate {
    source_route: PilotSourceRouteV1,
    source_id: String,
    source_revision: i64,
    full_text: String,
    content_sha256: String,
    kind: PilotRowKindV1,
    stratum: PilotStratumV1,
}

#[derive(Debug, Clone, Default)]
pub struct EligibleCounts {
    by_source: [usize; 2],
    by_kind: [usize; 2],
    by_stratum: [usize; 3],
    cells: [usize; 12],
}

impl EligibleCounts {
    fn from_candidates(candidates: &[EligibleCandidate]) -> Self {
        let mut counts = Self::default();
        for candidate in candidates {
            let route = route_index(candidate.source_route);
            let kind = kind_index(candidate.kind);
            let stratum = stratum_index(candidate.stratum);
            counts.by_source[route] += 1;
            counts.by_kind[kind] += 1;
            counts.by_stratum[stratum] += 1;
            counts.cells[cell_index(route, kind, stratum)] += 1;
        }
        counts
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct StratumAllocation {
    // antigravity narrative/control, then hapi narrative/control.
    cells: [usize; 4],
    imbalance: usize,
}

#[derive(Debug)]
struct SnapshotResolver {
    rows: BTreeMap<(PilotSourceRouteV1, String, i64), ResolvedPilotSourceV1>,
}

impl PilotSourceResolverV1 for SnapshotResolver {
    fn resolve_verified(
        &self,
        binding: &PilotRowV1,
    ) -> Result<ResolvedPilotSourceV1, SourceResolveError> {
        self.rows
            .get(&(
                binding.source_route,
                binding.source_id.clone(),
                binding.source_revision,
            ))
            .cloned()
            .ok_or_else(|| SourceResolveError::MissingExactRevision {
                source_route: binding.source_route,
                source_id: binding.source_id.clone(),
                source_revision: binding.source_revision,
            })
    }
}

/// Select, privacy-screen, verified-save, reload, and independently recheck
/// one exact real manifest. The caller separately fingerprints database files
/// before and after this operation to detect external mutation.
pub fn select_and_freeze_real_manifest_v1(
    config: &PilotSelectionConfigV1,
) -> Result<PilotSelectionReceiptV1, PilotSelectionErrorV1> {
    validate_output_path_identity(config)?;

    let mut candidates = Vec::new();
    candidates.extend(load_eligible_candidates(
        PilotSourceRouteV1::Antigravity,
        &config.antigravity_db,
    )?);
    candidates.extend(load_eligible_candidates(
        PilotSourceRouteV1::Hapi,
        &config.hapi_db,
    )?);

    let selected = select_exact_matrix(candidates)?;
    let rows = selected
        .iter()
        .map(|candidate| candidate_to_row(candidate, &config.capture_timestamp))
        .collect();
    let manifest = freeze_pilot_manifest(rows)
        .map_err(|errors| PilotSelectionErrorV1::Freeze(errors.len()))?;
    let digest = manifest
        .contract_digest()
        .map_err(|_| PilotSelectionErrorV1::Save)?;
    let resolver = SnapshotResolver {
        rows: selected
            .into_iter()
            .map(|candidate| {
                let key = (
                    candidate.source_route,
                    candidate.source_id.clone(),
                    candidate.source_revision,
                );
                let resolved = ResolvedPilotSourceV1 {
                    source_route: candidate.source_route,
                    source_id: candidate.source_id,
                    source_revision: candidate.source_revision,
                    full_text: candidate.full_text,
                };
                (key, resolved)
            })
            .collect(),
    };

    create_output_parent(&config.manifest_path)?;
    create_output_parent(&config.receipt_path)?;
    let manifest_staging = StagedArtifact::new(&config.manifest_path)?;
    let receipt_staging = StagedArtifact::new(&config.receipt_path)?;
    manifest
        .verify_and_save_to_path(manifest_staging.path(), &resolver)
        .map_err(|_| PilotSelectionErrorV1::Save)?;

    let loaded = PilotManifestV1::load_from_path(manifest_staging.path())
        .map_err(|_| PilotSelectionErrorV1::Reload)?;
    if loaded
        .contract_digest()
        .map_err(|_| PilotSelectionErrorV1::Reload)?
        != digest
    {
        return Err(PilotSelectionErrorV1::Reload);
    }
    independently_verify_manifest_sources(&loaded, &config.antigravity_db, &config.hapi_db)?;

    let receipt = receipt_from_manifest(&loaded, digest);
    write_public_receipt(receipt_staging.path(), &config.manifest_path, &receipt)?;
    // Recheck after verification to close a symlink/ancestor retarget window.
    // The manifest is the authority marker, so replace it last.
    validate_output_path_identity(config)?;
    receipt_staging.commit(&config.receipt_path)?;
    manifest_staging.commit(&config.manifest_path)?;
    Ok(receipt)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathIdentity {
    expected: PathBuf,
    file: Option<FileIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

fn validate_output_path_identity(
    config: &PilotSelectionConfigV1,
) -> Result<(), PilotSelectionErrorV1> {
    let sources = [
        path_identity(&config.antigravity_db)?,
        path_identity(&config.hapi_db)?,
    ];
    let outputs = [
        path_identity(&config.manifest_path)?,
        path_identity(&config.receipt_path)?,
    ];
    if paths_alias(&outputs[0], &outputs[1])
        || outputs
            .iter()
            .any(|output| sources.iter().any(|source| paths_alias(output, source)))
    {
        return Err(PilotSelectionErrorV1::OutputPathAlias);
    }
    Ok(())
}

fn path_identity(path: &Path) -> Result<PathIdentity, PilotSelectionErrorV1> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| PilotSelectionErrorV1::OutputPathIdentity)?
            .join(path)
    };
    Ok(PathIdentity {
        expected: canonicalize_expected(&absolute)
            .ok_or(PilotSelectionErrorV1::OutputPathIdentity)?,
        file: file_identity(&absolute),
    })
}

/// Adapted from `exec_env_reaper::canonicalize_expected`: resolve an existing
/// path normally, or canonicalize the deepest existing ancestor and rejoin a
/// normalized missing tail. This preserves symlink identity for outputs that
/// do not exist yet.
fn canonicalize_expected(path: &Path) -> Option<PathBuf> {
    if let Ok(real) = std::fs::canonicalize(path) {
        return Some(real);
    }
    let mut tail: Vec<OsString> = Vec::new();
    let mut cursor = path;
    loop {
        let parent = cursor.parent()?;
        tail.push(cursor.file_name()?.to_os_string());
        if let Ok(real_parent) = std::fs::canonicalize(parent) {
            let mut expected = real_parent;
            for component in tail.iter().rev() {
                expected.push(component);
            }
            return Some(normalize_absolute_lexically(&expected));
        }
        cursor = parent;
    }
}

fn normalize_absolute_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
        }
    }
    normalized
}

fn paths_alias(left: &PathIdentity, right: &PathIdentity) -> bool {
    left.expected == right.expected
        || left
            .file
            .zip(right.file)
            .is_some_and(|(left, right)| left == right)
}

#[cfg(unix)]
fn file_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    Some(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn file_identity(_path: &Path) -> Option<FileIdentity> {
    None
}

fn create_output_parent(path: &Path) -> Result<(), PilotSelectionErrorV1> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(PilotSelectionErrorV1::ReceiptWrite)?;
    }
    Ok(())
}

struct StagedArtifact {
    path: PathBuf,
}

impl StagedArtifact {
    fn new(destination: &Path) -> Result<Self, PilotSelectionErrorV1> {
        let parent = destination
            .parent()
            .ok_or(PilotSelectionErrorV1::OutputPathIdentity)?;
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(PilotSelectionErrorV1::OutputPathIdentity)?;
        Ok(Self {
            path: parent.join(format!(".{name}.{}.phase2a-tmp", uuid::Uuid::new_v4())),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn commit(self, destination: &Path) -> Result<(), PilotSelectionErrorV1> {
        std::fs::rename(&self.path, destination).map_err(PilotSelectionErrorV1::AtomicReplace)
    }
}

impl Drop for StagedArtifact {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn open_readonly_snapshot(
    route: PilotSourceRouteV1,
    path: &Path,
) -> Result<Connection, PilotSelectionErrorV1> {
    let uri = format!("file:{}?mode=ro&immutable=1", path.display());
    let conn = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| PilotSelectionErrorV1::ReadOnlyOpen(route))?;
    conn.execute_batch("PRAGMA query_only=ON; BEGIN DEFERRED TRANSACTION;")
        .map_err(|_| PilotSelectionErrorV1::ReadOnlyOpen(route))?;
    let query_only: i64 = conn
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .map_err(|_| PilotSelectionErrorV1::ReadOnlyInvariant(route))?;
    if query_only != 1 {
        return Err(PilotSelectionErrorV1::ReadOnlyInvariant(route));
    }
    Ok(conn)
}

fn load_eligible_candidates(
    route: PilotSourceRouteV1,
    path: &Path,
) -> Result<Vec<EligibleCandidate>, PilotSelectionErrorV1> {
    let conn = open_readonly_snapshot(route, path)?;
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    {
        let mut statement = conn
            .prepare(
                "SELECT id, revision, text FROM memories \
                 WHERE archived = 0 AND id IS NOT NULL AND length(trim(id)) > 0 \
                 AND revision > 0 AND text IS NOT NULL AND length(trim(text)) > 0",
            )
            .map_err(|_| PilotSelectionErrorV1::Query(route))?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|_| PilotSelectionErrorV1::Query(route))?;
        for row in rows {
            let (source_id, source_revision, full_text) =
                row.map_err(|_| PilotSelectionErrorV1::Query(route))?;
            if screen_manifest_metadata_for_public_pilot(&source_id).is_err()
                || screen_source_for_public_pilot(&full_text).is_err()
            {
                continue;
            }
            if !seen.insert((source_id.clone(), source_revision)) {
                return Err(PilotSelectionErrorV1::DuplicateBinding(route));
            }
            let Some((kind, stratum)) = classify_source(&full_text) else {
                continue;
            };
            candidates.push(EligibleCandidate {
                source_route: route,
                source_id,
                source_revision,
                content_sha256: format!("{:x}", Sha256::digest(full_text.as_bytes())),
                full_text,
                kind,
                stratum,
            });
        }
    }
    conn.execute_batch("COMMIT")
        .map_err(|_| PilotSelectionErrorV1::Query(route))?;
    Ok(candidates)
}

fn classify_source(text: &str) -> Option<(PilotRowKindV1, PilotStratumV1)> {
    // Privacy is deliberately first: rejected content cannot influence even
    // the eligible classification pool.
    screen_source_for_public_pilot(text).ok()?;
    let lower = text.to_ascii_lowercase();
    let structured_lines = lower
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with("- ")
                || line.starts_with("* ")
                || line.starts_with("# ")
                || line
                    .split_once(['.', ')'])
                    .is_some_and(|(prefix, _)| prefix.parse::<usize>().is_ok())
        })
        .count();
    let control_terms = count_terms(
        &lower,
        &[
            "checklist",
            "procedure",
            "invariant",
            "must ",
            "never ",
            "always ",
            "step ",
            "steps ",
            "command ",
            "workflow",
        ],
    );
    let kind = if structured_lines >= 2 || lower.contains("```") || control_terms >= 2 {
        PilotRowKindV1::StructuredControl
    } else {
        PilotRowKindV1::Narrative
    };

    let scores = [
        count_terms(
            &lower,
            &[
                "align",
                "bug",
                "contradic",
                "correct",
                "drift",
                "error",
                "fix",
                "mismatch",
                "regress",
                "root cause",
                "stale",
                "wrong",
            ],
        ),
        count_terms(
            &lower,
            &[
                "assert", "check", "diagnos", "evidence", "fail", "health", "recover", "retry",
                "rollback", "test", "validat", "verif",
            ],
        ),
        count_terms(
            &lower,
            &[
                "binding",
                "database",
                "dispatch",
                "identity",
                "ledger",
                "memory",
                "path",
                "provenance",
                "receipt",
                "route",
                "scope",
                "source",
                "stor",
                "workspace",
            ],
        ),
    ];
    let (index, score) = scores
        .into_iter()
        .enumerate()
        .max_by_key(|(index, score)| (*score, std::cmp::Reverse(*index)))?;
    if score == 0 {
        return None;
    }
    let stratum = match index {
        0 => PilotStratumV1::CorrectionAlignment,
        1 => PilotStratumV1::VerificationRecovery,
        _ => PilotStratumV1::RoutingStoreProvenance,
    };
    Some((kind, stratum))
}

fn count_terms(text: &str, terms: &[&str]) -> usize {
    terms.iter().filter(|term| text.contains(**term)).count()
}

fn select_exact_matrix(
    mut candidates: Vec<EligibleCandidate>,
) -> Result<Vec<EligibleCandidate>, PilotSelectionErrorV1> {
    candidates.sort_by_key(stable_candidate_key);
    let counts = EligibleCounts::from_candidates(&candidates);
    let allocation = find_allocation(&counts)
        .ok_or_else(|| PilotSelectionErrorV1::InsufficientMatrix(Box::new(counts.clone())))?;

    let mut cells: [Vec<EligibleCandidate>; 12] = std::array::from_fn(|_| Vec::new());
    for candidate in candidates {
        let index = cell_index(
            route_index(candidate.source_route),
            kind_index(candidate.kind),
            stratum_index(candidate.stratum),
        );
        cells[index].push(candidate);
    }
    let mut selected = Vec::with_capacity(50);
    for stratum in 0..3 {
        for route in 0..2 {
            for kind in 0..2 {
                let allocation_cell = route * 2 + kind;
                let take = allocation[stratum].cells[allocation_cell];
                selected.extend(cells[cell_index(route, kind, stratum)].drain(..take));
            }
        }
    }
    Ok(selected)
}

fn stable_candidate_key(candidate: &EligibleCandidate) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for value in [
        "lesson_forge_pilot_selection_v1",
        candidate.source_route.as_str(),
        candidate.source_id.as_str(),
        candidate.content_sha256.as_str(),
    ] {
        hasher.update(value.len().to_be_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.update(candidate.source_revision.to_be_bytes());
    hasher.finalize().into()
}

fn find_allocation(counts: &EligibleCounts) -> Option<[StratumAllocation; 3]> {
    let options: [Vec<StratumAllocation>; 3] = std::array::from_fn(|stratum| {
        let caps = [
            counts.cells[cell_index(0, 0, stratum)],
            counts.cells[cell_index(0, 1, stratum)],
            counts.cells[cell_index(1, 0, stratum)],
            counts.cells[cell_index(1, 1, stratum)],
        ];
        allocation_options(STRATUM_TARGETS[stratum], caps)
    });
    let mut third_by_margins = BTreeMap::new();
    for option in &options[2] {
        third_by_margins
            .entry((route_a_total(option), narrative_total(option)))
            .or_insert(*option);
    }

    let mut best: Option<(usize, [usize; 12], [StratumAllocation; 3])> = None;
    for first in &options[0] {
        for second in &options[1] {
            let used_route_a = route_a_total(first) + route_a_total(second);
            let used_narrative = narrative_total(first) + narrative_total(second);
            if used_route_a > SOURCE_TARGET || used_narrative > KIND_TARGET {
                continue;
            }
            let key = (SOURCE_TARGET - used_route_a, KIND_TARGET - used_narrative);
            let Some(third) = third_by_margins.get(&key) else {
                continue;
            };
            let allocations = [*first, *second, *third];
            let mut flat = [0usize; 12];
            for (stratum, allocation) in allocations.iter().enumerate() {
                flat[stratum * 4..stratum * 4 + 4].copy_from_slice(&allocation.cells);
            }
            let candidate = (
                allocations.iter().map(|value| value.imbalance).sum(),
                flat,
                allocations,
            );
            if best.as_ref().is_none_or(|current| candidate < *current) {
                best = Some(candidate);
            }
        }
    }
    best.map(|(_, _, allocation)| allocation)
}

fn allocation_options(target: usize, caps: [usize; 4]) -> Vec<StratumAllocation> {
    let mut options = Vec::new();
    for a_narrative in 0..=target.min(caps[0]) {
        for a_control in 0..=(target - a_narrative).min(caps[1]) {
            for h_narrative in 0..=(target - a_narrative - a_control).min(caps[2]) {
                let used = a_narrative + a_control + h_narrative;
                let h_control = target - used;
                if h_control > caps[3] {
                    continue;
                }
                let cells = [a_narrative, a_control, h_narrative, h_control];
                let imbalance = cells
                    .iter()
                    .map(|value| {
                        let delta = (*value as isize * 4) - target as isize;
                        (delta * delta) as usize
                    })
                    .sum();
                options.push(StratumAllocation { cells, imbalance });
            }
        }
    }
    options.sort_by_key(|option| (option.imbalance, option.cells));
    options
}

fn route_a_total(allocation: &StratumAllocation) -> usize {
    allocation.cells[0] + allocation.cells[1]
}

fn narrative_total(allocation: &StratumAllocation) -> usize {
    allocation.cells[0] + allocation.cells[2]
}

fn candidate_to_row(candidate: &EligibleCandidate, capture_timestamp: &str) -> PilotRowV1 {
    PilotRowV1 {
        source_route: candidate.source_route,
        source_id: candidate.source_id.clone(),
        source_revision: candidate.source_revision,
        content_sha256: candidate.content_sha256.clone(),
        capture_timestamp: capture_timestamp.to_string(),
        kind: candidate.kind,
        stratum: candidate.stratum,
        selection_reason: format!(
            "privacy-screened deterministic {} sample for the {} stratum",
            match candidate.kind {
                PilotRowKindV1::Narrative => "narrative",
                PilotRowKindV1::StructuredControl => "structured-control",
            },
            candidate.stratum.as_str()
        ),
        reference_decision: match candidate.stratum {
            PilotStratumV1::CorrectionAlignment => {
                "Evaluate whether the candidate preserves corrective intent and alignment constraints."
            }
            PilotStratumV1::VerificationRecovery => {
                "Evaluate whether the candidate improves verification or recovery decisions without unsupported claims."
            }
            PilotStratumV1::RoutingStoreProvenance => {
                "Evaluate whether the candidate preserves routing, storage, and provenance boundaries."
            }
        }
        .to_string(),
        target_kind: match (candidate.kind, candidate.stratum) {
            (PilotRowKindV1::Narrative, _) => LessonCandidateKindV1::Precedent,
            (PilotRowKindV1::StructuredControl, PilotStratumV1::CorrectionAlignment) => {
                LessonCandidateKindV1::BugClass
            }
            (PilotRowKindV1::StructuredControl, PilotStratumV1::VerificationRecovery) => {
                LessonCandidateKindV1::VerificationPattern
            }
            (PilotRowKindV1::StructuredControl, PilotStratumV1::RoutingStoreProvenance) => {
                LessonCandidateKindV1::LaneEvidence
            }
        },
    }
}

fn independently_verify_manifest_sources(
    manifest: &PilotManifestV1,
    antigravity_db: &Path,
    hapi_db: &Path,
) -> Result<(), PilotSelectionErrorV1> {
    for (route, path) in [
        (PilotSourceRouteV1::Antigravity, antigravity_db),
        (PilotSourceRouteV1::Hapi, hapi_db),
    ] {
        let conn = open_readonly_snapshot(route, path)?;
        for binding in manifest
            .rows()
            .iter()
            .filter(|row| row.source_route == route)
        {
            let resolved = conn
                .query_row(
                    "SELECT id, revision, text FROM memories \
                     WHERE id = ?1 AND revision = ?2 AND archived = 0 LIMIT 1",
                    (&binding.source_id, binding.source_revision),
                    |row| {
                        Ok(ResolvedPilotSourceV1 {
                            source_route: route,
                            source_id: row.get(0)?,
                            source_revision: row.get(1)?,
                            full_text: row.get(2)?,
                        })
                    },
                )
                .optional()
                .map_err(|_| PilotSelectionErrorV1::IndependentVerification(route))?
                .ok_or(PilotSelectionErrorV1::IndependentVerification(route))?;
            verify_resolved_source_v1(binding, &resolved)
                .map_err(|_| PilotSelectionErrorV1::IndependentVerification(route))?;
            screen_source_for_public_pilot(&resolved.full_text)
                .map_err(|_| PilotSelectionErrorV1::IndependentVerification(route))?;
        }
        conn.execute_batch("COMMIT")
            .map_err(|_| PilotSelectionErrorV1::IndependentVerification(route))?;
    }
    Ok(())
}

fn receipt_from_manifest(
    manifest: &PilotManifestV1,
    contract_digest: String,
) -> PilotSelectionReceiptV1 {
    let mut receipt = PilotSelectionReceiptV1 {
        contract_digest,
        rows: manifest.rows().len(),
        by_source: [0; 2],
        by_kind: [0; 2],
        by_stratum: [0; 3],
    };
    for row in manifest.rows() {
        receipt.by_source[route_index(row.source_route)] += 1;
        receipt.by_kind[kind_index(row.kind)] += 1;
        receipt.by_stratum[stratum_index(row.stratum)] += 1;
    }
    receipt
}

fn write_public_receipt(
    path: &Path,
    manifest_path: &Path,
    receipt: &PilotSelectionReceiptV1,
) -> Result<(), PilotSelectionErrorV1> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(PilotSelectionErrorV1::ReceiptWrite)?;
    }
    let manifest_name = manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("phase-2a-manifest.json");
    let markdown = format!(
        "# #1073 phase-2a manifest freeze receipt\n\n\
         - Status: real 50-row manifest selected and frozen; no model calls performed.\n\
         - Manifest: `{manifest_name}`\n\
         - Contract SHA-256: `{}`\n\
         - Privacy: every selected source passed the production privacy gate before verified save; no source text is persisted.\n\
         - Source access: strict read-only SQLite, `query_only`, stable transactions.\n\n\
         | Dimension | Counts |\n\
         | --- | --- |\n\
         | Rows | {} |\n\
         | Source routes | antigravity {}, hapi {} |\n\
         | Kinds | narrative {}, structured-control {} |\n\
         | Strata | correction/alignment {}, verification/recovery {}, routing/store/provenance {} |\n",
        receipt.contract_digest,
        receipt.rows,
        receipt.by_source[0],
        receipt.by_source[1],
        receipt.by_kind[0],
        receipt.by_kind[1],
        receipt.by_stratum[0],
        receipt.by_stratum[1],
        receipt.by_stratum[2],
    );
    std::fs::write(path, markdown).map_err(PilotSelectionErrorV1::ReceiptWrite)
}

const fn route_index(route: PilotSourceRouteV1) -> usize {
    match route {
        PilotSourceRouteV1::Antigravity => 0,
        PilotSourceRouteV1::Hapi => 1,
    }
}

const fn kind_index(kind: PilotRowKindV1) -> usize {
    match kind {
        PilotRowKindV1::Narrative => 0,
        PilotRowKindV1::StructuredControl => 1,
    }
}

const fn stratum_index(stratum: PilotStratumV1) -> usize {
    match stratum {
        PilotStratumV1::CorrectionAlignment => 0,
        PilotStratumV1::VerificationRecovery => 1,
        PilotStratumV1::RoutingStoreProvenance => 2,
    }
}

const fn cell_index(route: usize, kind: usize, stratum: usize) -> usize {
    route * 6 + kind * 3 + stratum
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_text(kind: usize, stratum: usize, index: usize) -> String {
        let topic = match stratum {
            0 => "A mismatch exposed a regression and the root cause required a correction.",
            1 => "A verification test recovered after retry and recorded validation evidence.",
            _ => "A dispatch route preserved database storage provenance and source identity.",
        };
        if kind == 0 {
            format!("Synthetic narrative {index}. {topic}")
        } else {
            format!("Synthetic control {index}.\n- first check\n- second check\n{topic}")
        }
    }

    fn fixture_db(route_tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "sigil-1073-selection-{route_tag}-{}.db",
            uuid::Uuid::new_v4()
        ));
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE; \
             CREATE TABLE memories (id TEXT, revision INTEGER, text TEXT, archived INTEGER);",
        )
        .unwrap();
        for kind in 0..2 {
            for stratum in 0..3 {
                for index in 0..10 {
                    let id = format!("synthetic-{route_tag}-{kind}-{stratum}-{index}");
                    let text = fixture_text(kind, stratum, index);
                    conn.execute(
                        "INSERT INTO memories (id, revision, text, archived) VALUES (?1, 1, ?2, 0)",
                        (&id, &text),
                    )
                    .unwrap();
                }
            }
        }
        drop(conn);
        path
    }

    fn temp_artifact(extension: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sigil-1073-selection-{}.{}",
            uuid::Uuid::new_v4(),
            extension
        ))
    }

    fn config_with_outputs(
        antigravity: &Path,
        hapi: &Path,
        manifest: PathBuf,
        receipt: PathBuf,
    ) -> PilotSelectionConfigV1 {
        PilotSelectionConfigV1 {
            antigravity_db: antigravity.to_path_buf(),
            hapi_db: hapi.to_path_buf(),
            capture_timestamp: "2026-07-24T04:00:00Z".to_string(),
            manifest_path: manifest,
            receipt_path: receipt,
        }
    }

    fn relative_path(from: &Path, to: &Path) -> PathBuf {
        let from: Vec<_> = from.components().collect();
        let to: Vec<_> = to.components().collect();
        let common = from
            .iter()
            .zip(&to)
            .take_while(|(left, right)| left == right)
            .count();
        let mut relative = PathBuf::new();
        for _ in common..from.len() {
            relative.push("..");
        }
        for component in &to[common..] {
            relative.push(component.as_os_str());
        }
        relative
    }

    #[test]
    fn privacy_rejection_precedes_classification() {
        let unsafe_text = "- first check\n- second check\nsk-abcdefghijklmnopqrstuvwxyz123456";
        assert!(classify_source(unsafe_text).is_none());
    }

    #[test]
    fn output_alias_never_corrupts_a_synthetic_source_database() {
        let antigravity = fixture_db("alias-a");
        let hapi = fixture_db("alias-h");
        let receipt_path = temp_artifact("md");
        let before = std::fs::read(&antigravity).unwrap();
        let config = PilotSelectionConfigV1 {
            antigravity_db: antigravity.clone(),
            hapi_db: hapi.clone(),
            capture_timestamp: "2026-07-24T04:00:00Z".to_string(),
            manifest_path: antigravity.clone(),
            receipt_path: receipt_path.clone(),
        };
        assert!(select_and_freeze_real_manifest_v1(&config).is_err());
        let after = std::fs::read(&antigravity).unwrap();
        for path in [antigravity, hapi, receipt_path] {
            let _ = std::fs::remove_file(path);
        }
        assert!(
            after == before,
            "an output alias must be refused before a source can be overwritten"
        );
    }

    #[test]
    fn alias_refusal_precedes_any_sqlite_source_read() {
        let antigravity = temp_artifact("db");
        let hapi = temp_artifact("db");
        std::fs::write(&antigravity, b"not a sqlite database").unwrap();
        std::fs::write(&hapi, b"not a sqlite database").unwrap();
        let config = config_with_outputs(
            &antigravity,
            &hapi,
            antigravity.clone(),
            temp_artifact("md"),
        );
        let error = select_and_freeze_real_manifest_v1(&config).unwrap_err();
        for path in [antigravity, hapi] {
            let _ = std::fs::remove_file(path);
        }
        assert!(matches!(error, PilotSelectionErrorV1::OutputPathAlias));
    }

    #[test]
    fn lexical_relative_absolute_and_output_output_aliases_are_refused() {
        let antigravity = fixture_db("path-a");
        let hapi = fixture_db("path-h");
        let receipt = temp_artifact("md");
        let exact = config_with_outputs(&antigravity, &hapi, antigravity.clone(), receipt.clone());
        assert!(matches!(
            validate_output_path_identity(&exact),
            Err(PilotSelectionErrorV1::OutputPathAlias)
        ));

        let relative = relative_path(&std::env::current_dir().unwrap(), &antigravity);
        let relative_absolute = config_with_outputs(&antigravity, &hapi, relative, receipt.clone());
        assert!(matches!(
            validate_output_path_identity(&relative_absolute),
            Err(PilotSelectionErrorV1::OutputPathAlias)
        ));

        let receipt_source =
            config_with_outputs(&antigravity, &hapi, temp_artifact("json"), hapi.clone());
        assert!(matches!(
            validate_output_path_identity(&receipt_source),
            Err(PilotSelectionErrorV1::OutputPathAlias)
        ));

        let output_output =
            config_with_outputs(&antigravity, &hapi, receipt.clone(), receipt.clone());
        assert!(matches!(
            validate_output_path_identity(&output_output),
            Err(PilotSelectionErrorV1::OutputPathAlias)
        ));
        for path in [antigravity, hapi] {
            let _ = std::fs::remove_file(path);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_dotdot_and_hard_link_aliases_are_refused() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "sigil-1073-selection-alias-root-{}",
            uuid::Uuid::new_v4()
        ));
        let target = root.join("target");
        let subdirectory = target.join("subdirectory");
        std::fs::create_dir_all(&subdirectory).unwrap();
        let original = fixture_db("symlink-a");
        let antigravity = target.join("memory.db");
        std::fs::rename(original, &antigravity).unwrap();
        let hapi = fixture_db("symlink-h");
        let link = root.join("link");
        symlink(&subdirectory, &link).unwrap();
        let through_dotdot = link.join("..").join("memory.db");
        let symlink_config =
            config_with_outputs(&antigravity, &hapi, through_dotdot, temp_artifact("md"));
        assert!(matches!(
            validate_output_path_identity(&symlink_config),
            Err(PilotSelectionErrorV1::OutputPathAlias)
        ));

        let hard_link = root.join("hard-link.db");
        std::fs::hard_link(&antigravity, &hard_link).unwrap();
        let hard_link_config =
            config_with_outputs(&antigravity, &hapi, hard_link, temp_artifact("md"));
        assert!(matches!(
            validate_output_path_identity(&hard_link_config),
            Err(PilotSelectionErrorV1::OutputPathAlias)
        ));
        let _ = std::fs::remove_file(hapi);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn nonexistent_outputs_resolve_through_their_deepest_existing_ancestor() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "sigil-1073-selection-missing-root-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root_alias = root.with_extension("alias");
        symlink(&root, &root_alias).unwrap();
        let antigravity = fixture_db("missing-a");
        let hapi = fixture_db("missing-h");
        let manifest = root.join("missing").join("artifact.json");
        let receipt = root_alias.join("missing").join("artifact.json");
        let config = config_with_outputs(&antigravity, &hapi, manifest, receipt);
        assert!(matches!(
            validate_output_path_identity(&config),
            Err(PilotSelectionErrorV1::OutputPathAlias)
        ));
        assert!(!root.join("missing").exists());
        for path in [antigravity, hapi] {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_file(root_alias);
        let _ = std::fs::remove_dir(root);
    }

    #[test]
    fn unsafe_source_ids_never_enter_the_eligible_pool() {
        let database = fixture_db("unsafe-id");
        let conn = Connection::open(&database).unwrap();
        conn.execute(
            "INSERT INTO memories (id, revision, text, archived) VALUES (?1, 1, ?2, 0)",
            (
                "sk-abcdefghijklmnopqrstuvwxyz123456",
                fixture_text(0, 0, 99),
            ),
        )
        .unwrap();
        drop(conn);
        let eligible = load_eligible_candidates(PilotSourceRouteV1::Antigravity, &database)
            .expect("synthetic source must remain readable");
        let _ = std::fs::remove_file(database);
        assert_eq!(eligible.len(), 60);
    }

    #[test]
    fn deterministic_selector_fills_the_exact_frozen_matrix() {
        let antigravity = fixture_db("a");
        let hapi = fixture_db("h");
        let manifest_path = temp_artifact("json");
        let receipt_path = temp_artifact("md");
        let before_a = std::fs::read(&antigravity).unwrap();
        let before_h = std::fs::read(&hapi).unwrap();
        let config = PilotSelectionConfigV1 {
            antigravity_db: antigravity.clone(),
            hapi_db: hapi.clone(),
            capture_timestamp: "2026-07-24T04:00:00Z".to_string(),
            manifest_path: manifest_path.clone(),
            receipt_path: receipt_path.clone(),
        };
        let receipt = select_and_freeze_real_manifest_v1(&config).unwrap();
        assert_eq!(receipt.rows, 50);
        assert_eq!(receipt.by_source, [25, 25]);
        assert_eq!(receipt.by_kind, [25, 25]);
        assert_eq!(receipt.by_stratum, [17, 17, 16]);
        assert_eq!(std::fs::read(&antigravity).unwrap(), before_a);
        assert_eq!(std::fs::read(&hapi).unwrap(), before_h);

        let first = std::fs::read(&manifest_path).unwrap();
        select_and_freeze_real_manifest_v1(&config).unwrap();
        assert_eq!(std::fs::read(&manifest_path).unwrap(), first);
        let public = String::from_utf8(first).unwrap();
        assert!(!public.contains("Synthetic narrative"));
        assert!(!public.contains("Synthetic control"));

        for path in [antigravity, hapi, manifest_path, receipt_path] {
            let _ = std::fs::remove_file(path);
        }
    }
}

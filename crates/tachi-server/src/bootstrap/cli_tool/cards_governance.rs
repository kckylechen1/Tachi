//! Filesystem orchestration for reviewed lane-card artifacts (#1306).
//!
//! A same-directory advisory flock coordinates Tachi writers. External
//! editors do not honor it; source revalidation immediately before atomic
//! rename limits, but cannot eliminate, that non-cooperating race.

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tachi_params::{
    hash_bytes, hash_json, validate_fresh_evidence, verify_approval, ApprovalArtifact,
    ApprovalRequest, DraftRequest, EvidenceSnapshot, ReviewDecision, ReviewRequest,
    GOVERNANCE_VERSION,
};

use super::super::print_pretty_json;

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, Box<dyn std::error::Error>> {
    Ok(serde_json::from_slice(
        &std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?,
    )?)
}
fn output<T: Serialize>(value: &T) -> Result<(), Box<dyn std::error::Error>> {
    print_pretty_json(&serde_json::to_value(value)?)
}

pub(super) fn draft_command(input: &Path) -> Result<(), Box<dyn std::error::Error>> {
    output(&tachi_params::draft_lane_card(read_json::<DraftRequest>(
        input,
    )?)?)
}
pub(super) fn review_command(input: &Path) -> Result<(), Box<dyn std::error::Error>> {
    output(&tachi_params::review_lane_card(
        read_json::<ReviewRequest>(input)?,
    )?)
}

fn canonical_path(dir: &Path, seat: &str) -> PathBuf {
    dir.join(format!("{seat}.md"))
}
fn read_canonical(
    dir: &Path,
    seat: &str,
) -> Result<(PathBuf, Vec<u8>), Box<dyn std::error::Error>> {
    let path = canonical_path(dir, seat);
    let meta = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("canonical card {} must already exist: {e}", path.display()))?;
    if !meta.file_type().is_file() || meta.file_type().is_symlink() {
        return Err("canonical card must be a regular non-symlink file".into());
    }
    let bytes = std::fs::read(&path)?;
    std::str::from_utf8(&bytes).map_err(|_| "canonical card must be strict UTF-8")?;
    Ok((path, bytes))
}

fn validate_complete_projection(bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let text = std::str::from_utf8(bytes).map_err(|_| "canonical result must be UTF-8")?;
    let projected = super::cards_ledger::extract_counter_clauses(text)
        .ok_or("approved append did not produce a counter-clause projection")?;
    if !crate::dispatch_ops::complete_counter_clause_projection(&projected) {
        return Err(
            "approved append would truncate an atomic counter-clause projection; approval refused"
                .into(),
        );
    }
    Ok(())
}

pub(super) fn approve_command(input: &Path, dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let req: ApprovalRequest = read_json(input)?;
    if req.schema_version != GOVERNANCE_VERSION {
        return Err("unsupported governance schema_version".into());
    }
    tachi_params::verify_draft(&req.draft)?;
    tachi_params::verify_review(&req.review)?;
    if req.review.decision != ReviewDecision::Accepted {
        return Err("only an accepted review can be approved".into());
    }
    if req.review.draft_hash != req.draft.draft_hash
        || req.review.evidence_hash != req.draft.evidence_hash
    {
        return Err("review is not bound to this draft/evidence".into());
    }
    if req.leader.is_empty() || req.leader.trim() != req.leader {
        return Err("leader must not be empty".into());
    }
    if req.draft.author == req.review.reviewer
        || req.leader == req.draft.author
        || req.leader == req.review.reviewer
    {
        return Err("author, reviewer, and leader must be three distinct asserted actors".into());
    }
    let (_, source) = read_canonical(dir, &req.draft.seat)?;
    let append = req.draft.append_markdown.as_bytes().to_vec();
    let mut result = source.clone();
    result.extend_from_slice(&append);
    validate_complete_projection(&result)?;
    let mut out = ApprovalArtifact {
        schema_version: GOVERNANCE_VERSION.into(),
        draft: req.draft,
        review: req.review,
        leader: req.leader,
        decision: "approved".into(),
        source_hash: hash_bytes(&source),
        source_byte_len: source.len() as u64,
        append_offset: source.len() as u64,
        append_bytes_hash: hash_bytes(&append),
        append_bytes: append,
        expected_result_hash: hash_bytes(&result),
        approval_hash: String::new(),
    };
    out.approval_hash = hash_json(&out)?;
    verify_approval(&out)?;
    output(&out)
}

#[derive(Debug)]
pub(super) struct CardLock {
    _file: std::fs::File,
}
pub(super) fn lock(path: &Path) -> Result<CardLock, Box<dyn std::error::Error>> {
    let p = path.with_extension("md.tachi.lock");
    if let Ok(meta) = std::fs::symlink_metadata(&p) {
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            return Err("card lock must be a regular non-symlink file".into());
        }
    }
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&p)
        .map_err(|e| format!("acquire cooperating card lock {}: {e}", p.display()))?;
    let meta = f.metadata()?;
    let path_meta = std::fs::symlink_metadata(&p)?;
    if !meta.is_file() || path_meta.file_type().is_symlink() || !path_meta.file_type().is_file() {
        return Err("card lock must be a regular non-symlink file".into());
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the fd belongs to the live File retained by CardLock.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!(
                "acquire cooperating card lock {}: {}",
                p.display(),
                std::io::Error::last_os_error()
            )
            .into());
        }
    }
    #[cfg(not(unix))]
    return Err("card apply locking is unsupported on this platform; refusing write".into());
    #[cfg(unix)]
    Ok(CardLock { _file: f })
}

#[derive(Debug)]
pub(super) struct ApplySourceOutcome {
    pub seat: String,
    pub source_hash: String,
    pub source_status: &'static str,
    // Keeps the cooperating writer lock alive through mirror verification.
    _lock: CardLock,
}
pub(super) fn apply_command(
    approval_path: &Path,
    evidence_path: &Path,
    dir: &Path,
) -> Result<ApplySourceOutcome, Box<dyn std::error::Error>> {
    let approval: ApprovalArtifact = read_json(approval_path)?;
    let fresh: Vec<EvidenceSnapshot> = read_json(evidence_path)?;
    verify_approval(&approval)?;
    if approval
        .draft
        .evidence_pins
        .iter()
        .filter(|e| e.relation == tachi_params::EvidenceRelation::Supports)
        .any(|e| {
            e.subject_role != approval.draft.role
                || e.subject_vendor != approval.draft.vendor
                || e.subject_agent != approval.draft.agent
        })
    {
        return Err("supporting evidence no longer matches approved role/vendor/agent".into());
    }
    validate_fresh_evidence(&approval.draft.evidence_pins, &fresh)?;
    let (path, _) = read_canonical(dir, &approval.draft.seat)?;
    let lock = lock(&path)?;
    let (_, current) = read_canonical(dir, &approval.draft.seat)?;
    let marker = format!(
        "{}{} -->",
        tachi_params::ENTRY_MARKER_PREFIX,
        approval.draft.dedupe_key
    );
    let marker_positions: Vec<usize> = current
        .windows(marker.len())
        .enumerate()
        .filter_map(|(i, bytes)| (bytes == marker.as_bytes()).then_some(i))
        .collect();
    let exact_start = current
        .windows(approval.append_bytes.len())
        .position(|bytes| bytes == approval.append_bytes);
    if let Some(start) = exact_start {
        let expected_marker = start
            + approval
                .append_bytes
                .windows(marker.len())
                .position(|b| b == marker.as_bytes())
                .ok_or("canonical append lacks marker")?;
        if marker_positions.len() != 1 || marker_positions[0] != expected_marker {
            return Err("dedupe marker conflict in canonical card".into());
        }
        return Ok(ApplySourceOutcome {
            seat: approval.draft.seat,
            source_hash: hash_bytes(&current),
            source_status: "already_applied",
            _lock: lock,
        });
    }
    if !marker_positions.is_empty() {
        return Err("dedupe marker exists with differing or partial entry bytes".into());
    }
    if current.len() != approval.source_byte_len as usize
        || hash_bytes(&current) != approval.source_hash
    {
        return Err("stale canonical card; zero write".into());
    }
    let mut result = current;
    result.extend_from_slice(&approval.append_bytes);
    if hash_bytes(&result) != approval.expected_result_hash {
        return Err("expected result hash mismatch".into());
    }
    validate_complete_projection(&result)?;
    crate::utils::write_owner_only_file_atomic(&path, &result)?;
    Ok(ApplySourceOutcome {
        seat: approval.draft.seat,
        source_hash: hash_bytes(&result),
        source_status: "applied",
        _lock: lock,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tachi_params::{
        draft_lane_card, review_lane_card, DraftRequest, EvidenceRelation, EvidenceState,
        LaneAuthority, ReviewReceipt,
    };

    fn chain(source: &[u8]) -> (ApprovalArtifact, Vec<EvidenceSnapshot>) {
        let evidence = vec![EvidenceSnapshot {
            id: "ev-1".into(),
            subject_role: "reviewer".into(),
            subject_vendor: "openai".into(),
            subject_agent: Some("codex".into()),
            source_ref: "run/1".into(),
            source_kind: "dispatch_run".into(),
            immutable_revision: "commit:1".into(),
            assertion_hash: "blake2:1".into(),
            relation: EvidenceRelation::Supports,
            state: EvidenceState::Current,
        }];
        let draft = draft_lane_card(DraftRequest {
            schema_version: GOVERNANCE_VERSION.into(),
            seat: "codex-review".into(),
            role: "reviewer".into(),
            vendor: "openai".into(),
            agent: Some("codex".into()),
            author: "author".into(),
            observed_at: "2026-07-19".into(),
            observed_failure_or_capability: "missed guard".into(),
            recurrence_context: "twice".into(),
            counter_clause: "Verify the exact source before writing.".into(),
            authority: LaneAuthority::LaneOperationalEvidence,
            evidence: evidence.clone(),
        })
        .unwrap();
        let review: ReviewReceipt = review_lane_card(ReviewRequest {
            schema_version: GOVERNANCE_VERSION.into(),
            draft: draft.clone(),
            reviewer: "reviewer".into(),
            decision: ReviewDecision::Accepted,
            notes: "accepted".into(),
        })
        .unwrap();
        let append = draft.append_markdown.as_bytes().to_vec();
        let mut result = source.to_vec();
        result.extend_from_slice(&append);
        let mut approval = ApprovalArtifact {
            schema_version: GOVERNANCE_VERSION.into(),
            draft,
            review,
            leader: "leader".into(),
            decision: "approved".into(),
            source_hash: hash_bytes(source),
            source_byte_len: source.len() as u64,
            append_offset: source.len() as u64,
            append_bytes_hash: hash_bytes(&append),
            append_bytes: append,
            expected_result_hash: hash_bytes(&result),
            approval_hash: String::new(),
        };
        approval.approval_hash = hash_json(&approval).unwrap();
        (approval, evidence)
    }

    fn write_inputs(
        dir: &Path,
        approval: &ApprovalArtifact,
        evidence: &[EvidenceSnapshot],
    ) -> (PathBuf, PathBuf) {
        let a = dir.join("approval.json");
        let e = dir.join("evidence.json");
        std::fs::write(&a, serde_json::to_vec(approval).unwrap()).unwrap();
        std::fs::write(&e, serde_json::to_vec(evidence).unwrap()).unwrap();
        (a, e)
    }

    #[test]
    fn apply_appends_exactly_once_even_after_unrelated_later_append() {
        let td = tempfile::tempdir().unwrap();
        let source = b"# card\n";
        std::fs::write(td.path().join("codex-review.md"), source).unwrap();
        let (approval, evidence) = chain(source);
        let (a, e) = write_inputs(td.path(), &approval, &evidence);
        assert_eq!(
            apply_command(&a, &e, td.path()).unwrap().source_status,
            "applied"
        );
        let expected = [source.as_slice(), approval.append_bytes.as_slice()].concat();
        assert_eq!(
            std::fs::read(td.path().join("codex-review.md")).unwrap(),
            expected
        );
        assert_eq!(
            apply_command(&a, &e, td.path()).unwrap().source_status,
            "already_applied"
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(td.path().join("codex-review.md"))
            .unwrap()
            .write_all(b"\nunrelated later append\n")
            .unwrap();
        assert_eq!(
            apply_command(&a, &e, td.path()).unwrap().source_status,
            "already_applied"
        );
        assert_eq!(
            std::fs::read(td.path().join("codex-review.md")).unwrap(),
            [expected, b"\nunrelated later append\n".to_vec()].concat()
        );
    }

    #[test]
    fn reapproval_at_new_eof_dedupes_and_marker_conflicts_fail() {
        let td = tempfile::tempdir().unwrap();
        let source = b"# card\n";
        let card = td.path().join("codex-review.md");
        std::fs::write(&card, source).unwrap();
        let (first, evidence) = chain(source);
        let (a, e) = write_inputs(td.path(), &first, &evidence);
        apply_command(&a, &e, td.path()).unwrap();
        let once = std::fs::read(&card).unwrap();

        let (second, _) = chain(&once);
        let (a2, e2) = write_inputs(td.path(), &second, &evidence);
        assert_eq!(
            apply_command(&a2, &e2, td.path()).unwrap().source_status,
            "already_applied"
        );
        assert_eq!(std::fs::read(&card).unwrap(), once);

        let marker = format!(
            "{}{} -->",
            tachi_params::ENTRY_MARKER_PREFIX,
            first.draft.dedupe_key
        );
        std::fs::write(&card, format!("# card\n{marker}\npartial/different\n")).unwrap();
        assert!(apply_command(&a, &e, td.path())
            .unwrap_err()
            .to_string()
            .contains("marker"));
    }

    #[test]
    fn stale_and_tampered_chains_refuse_without_writing() {
        let td = tempfile::tempdir().unwrap();
        let source = b"# card\n";
        let card = td.path().join("codex-review.md");
        std::fs::write(&card, b"changed").unwrap();
        let (approval, evidence) = chain(source);
        let (a, e) = write_inputs(td.path(), &approval, &evidence);
        assert!(apply_command(&a, &e, td.path()).is_err());
        assert_eq!(std::fs::read(&card).unwrap(), b"changed");
        std::fs::write(&card, source).unwrap();
        for mutate in 0..4 {
            let mut bad = approval.clone();
            match mutate {
                0 => bad.draft.author.push('x'),
                1 => bad.review.notes.push('x'),
                2 => bad.append_bytes.push(b'x'),
                _ => bad.approval_hash.push('x'),
            }
            let (ba, _) = write_inputs(td.path(), &bad, &evidence);
            assert!(apply_command(&ba, &e, td.path()).is_err());
            assert_eq!(std::fs::read(&card).unwrap(), source);
        }
        for state in [EvidenceState::Corrected, EvidenceState::Retracted] {
            let mut bad_evidence = evidence.clone();
            bad_evidence[0].state = state;
            let (_, be) = write_inputs(td.path(), &approval, &bad_evidence);
            assert!(apply_command(&a, &be, td.path()).is_err());
        }
        let (_, missing) = write_inputs(td.path(), &approval, &[]);
        assert!(apply_command(&a, &missing, td.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn apply_refuses_symlink_target() {
        use std::os::unix::fs::symlink;
        let td = tempfile::tempdir().unwrap();
        let outside = td.path().join("outside");
        std::fs::write(&outside, b"# card\n").unwrap();
        symlink(&outside, td.path().join("codex-review.md")).unwrap();
        let (approval, evidence) = chain(b"# card\n");
        let (a, e) = write_inputs(td.path(), &approval, &evidence);
        assert!(apply_command(&a, &e, td.path()).is_err());
        assert_eq!(std::fs::read(outside).unwrap(), b"# card\n");
    }
}

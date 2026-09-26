//! Local operator Wiki review: read-only preview, then explicit apply.
//!
//! Canon doc §7 (`issue-refinery-memory-lanes.md`): "an independent review
//! and approval receipt are required before invoking close_loop, writing
//! active wiki, or posting GitHub writeback." Ordinary facades
//! (`tachi_wiki_write`, `tachi_save kind=wiki`, generic `tachi_memory`
//! saves onto `/wiki/...` paths) cannot mint that receipt — they strip
//! caller-supplied review authority and stamp `pending_review`. This
//! module is the operator seam that CAN: a local CLI
//! (`tachi wiki review`) with no MCP action, no daemon route, and no
//! network access.
//!
//! Two phases, both binding the same canonical digest:
//!
//! * **Preview** (`preview_wiki_review`): resolves the named Wiki store
//!   without creating or migrating anything, rereads the entry, verifies
//!   the operator's source manifest against the entry's references and
//!   the local snapshot bytes, and reports the exact revision + review
//!   digest an apply would bind. Strictly read-only.
//! * **Apply** (`apply_wiki_review`): revalidates the manifest and
//!   snapshots, rereads the row inside the identity-checked store
//!   closure, re-derives the digest, and only then performs the approved
//!   metadata write through the store's verified-write primitive
//!   (`update_with_revision_if_expected_state`), bumps the revision, and
//!   invalidates the recall cache.
//!
//! Honesty boundaries documented here and surfaced in CLI output:
//!
//! * URL / GitHub-shorthand references map to **operator-attested local
//!   snapshots**. This seam verifies the snapshot's bytes against the
//!   manifest's expected sha256; it does NOT fetch or verify live
//!   upstream content. Local absolute-path references must map to the
//!   referenced file itself — an unrelated snapshot substitution is
//!   refused.
//! * Validation observes snapshot bytes at validation time. There is no
//!   atomicity claim across the manifest files, the snapshot files, and
//!   the database write; the review digest binds what was observed.

use super::*;
use memcore::ExpectedMemoryState;
use rusqlite::{params, OptionalExtension};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::io::Read;

/// Domain separators for the two canonical hashes this module derives.
pub(crate) const WIKI_REVIEW_DIGEST_DOMAIN: &str = "tachi:wiki-review:v1";
pub(crate) const WIKI_SOURCE_BUNDLE_DOMAIN: &str = "tachi:wiki-source-bundle:v1";

/// Snapshot hashing streams in bounded chunks; no unbounded `read_to_end`
/// on operator-supplied paths.
const SNAPSHOT_HASH_BUFFER_BYTES: usize = 64 * 1024;

const SNAPSHOT_VERIFICATION_REFERENCED_FILE: &str = "referenced_file";
const SNAPSHOT_VERIFICATION_OPERATOR_ATTESTED: &str = "operator_attested_snapshot";

// ─── source manifest ────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct WikiSourceManifestRow {
    #[serde(rename = "ref")]
    target_ref: String,
    snapshot: PathBuf,
    sha256: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct WikiSourceManifest {
    #[serde(default)]
    version: Option<u64>,
    sources: Vec<WikiSourceManifestRow>,
}

fn canonical_reference_list(references: &[String]) -> Vec<String> {
    let mut canonical = references
        .iter()
        .map(|reference| reference.trim())
        .filter(|reference| !reference.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    canonical.sort();
    canonical.dedup();
    canonical
}

fn normalize_expected_sha256(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    if trimmed.len() != 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!(
            "manifest sha256 '{raw}' must be exactly 64 hex characters"
        ));
    }
    Ok(trimmed)
}

fn load_source_manifest(path: &Path) -> Result<WikiSourceManifest, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect source manifest {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(format!(
            "source manifest {} must be a regular non-symlink file",
            path.display()
        ));
    }
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("cannot open source manifest {}: {error}", path.display()))?;
        read_bounded(&mut file, path)?
    };
    #[cfg(not(unix))]
    let bytes = {
        let mut file = std::fs::File::open(path)
            .map_err(|e| format!("cannot open source manifest {}: {e}", path.display()))?;
        read_bounded(&mut file, path)?
    };
    let manifest: WikiSourceManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid source manifest {}: {error}", path.display()))?;
    if let Some(version) = manifest.version {
        if version != 1 {
            return Err(format!(
                "unsupported source manifest version {version} (expected 1)"
            ));
        }
    }
    if manifest.sources.is_empty() {
        return Err(format!(
            "source manifest {} has an empty sources array",
            path.display()
        ));
    }
    Ok(manifest)
}

fn read_bounded(reader: &mut impl Read, path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = vec![0_u8; SNAPSHOT_HASH_BUFFER_BYTES];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if read == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(format!(
                "{} exceeds the 64 MiB source-snapshot size bound",
                path.display()
            ));
        }
    }
}

fn is_absolute_path_reference(reference: &str) -> bool {
    let trimmed = reference.trim();
    if trimmed.starts_with('/') {
        return true;
    }
    // Windows drive path `X:\...` / `X:/...`, mirroring
    // `wiki_ops::references`' validation vocabulary without its regex.
    let bytes = trimmed.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
}

fn open_snapshot_file(path: &Path) -> Result<std::fs::File, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("cannot open snapshot {}: {error}", path.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::File::open(path)
            .map_err(|error| format!("cannot open snapshot {}: {error}", path.display()))
    }
}

/// One verified manifest row: the snapshot was a regular file whose
/// streamed sha256 matched the manifest's expectation.
#[derive(Debug, Clone)]
pub(crate) struct VerifiedSourceRecord {
    target_ref: String,
    snapshot: PathBuf,
    sha256: String,
    verification: &'static str,
}

impl VerifiedSourceRecord {
    fn digest_json(&self) -> Value {
        json!({
            "ref": self.target_ref,
            "sha256": self.sha256,
        })
    }

    fn preview_json(&self) -> Value {
        json!({
            "ref": self.target_ref,
            "snapshot": self.snapshot.display().to_string(),
            "sha256": self.sha256,
            "verification": self.verification,
        })
    }
}

fn verify_source_manifest(
    manifest: &WikiSourceManifest,
    entry_references: &[String],
) -> Result<Vec<VerifiedSourceRecord>, String> {
    let entry_refs = canonical_reference_list(entry_references);
    if entry_refs.is_empty() {
        return Err(
            "the entry carries no references; an approval requires exact nonempty \
             reference coverage in the source manifest"
                .to_string(),
        );
    }

    let mut seen_refs = BTreeMap::<String, ()>::new();
    let mut rows = BTreeMap::<String, WikiSourceManifestRow>::new();
    for row in &manifest.sources {
        let target_ref = row.target_ref.trim().to_string();
        if target_ref.is_empty() {
            return Err("manifest rows must carry a nonempty ref".to_string());
        }
        if seen_refs.insert(target_ref.clone(), ()).is_some() {
            return Err(format!(
                "duplicate manifest row for reference '{target_ref}'"
            ));
        }
        if row.snapshot.as_os_str().is_empty() {
            return Err(format!(
                "manifest row for reference '{target_ref}' must carry a snapshot path"
            ));
        }
        let expected_sha256 = normalize_expected_sha256(&row.sha256)
            .map_err(|error| format!("reference '{target_ref}': {error}"))?;
        rows.insert(
            target_ref,
            WikiSourceManifestRow {
                target_ref: row.target_ref.clone(),
                snapshot: row.snapshot.clone(),
                sha256: expected_sha256,
            },
        );
    }

    // Exact coverage: every entry reference present, no unmatched rows.
    let mut missing = entry_refs
        .iter()
        .filter(|reference| !rows.contains_key(*reference))
        .map(String::as_str)
        .collect::<Vec<_>>();
    missing.sort();
    if !missing.is_empty() {
        return Err(format!(
            "source manifest is missing rows for entry references: {}",
            missing.join(", ")
        ));
    }
    let mut unmatched = rows
        .keys()
        .filter(|reference| !entry_refs.contains(reference))
        .map(String::as_str)
        .collect::<Vec<_>>();
    unmatched.sort();
    if !unmatched.is_empty() {
        return Err(format!(
            "source manifest carries rows that match no entry reference: {}",
            unmatched.join(", ")
        ));
    }

    let mut records = Vec::new();
    for reference in &entry_refs {
        let row = rows.get(reference).expect("coverage checked above");
        // The snapshot must be a regular, non-symlink file — directories,
        // devices, fifos, and symlink indirections are all refused.
        let snapshot_metadata = std::fs::symlink_metadata(&row.snapshot).map_err(|error| {
            format!(
                "cannot inspect snapshot {} for reference '{reference}': {error}",
                row.snapshot.display()
            )
        })?;
        if snapshot_metadata.file_type().is_symlink() || !snapshot_metadata.file_type().is_file() {
            return Err(format!(
                "snapshot {} for reference '{reference}' must be a regular non-symlink file",
                row.snapshot.display()
            ));
        }
        // A local absolute-path reference names the exact file that must
        // be hashed; substituting an unrelated local snapshot is refused.
        let verification = if is_absolute_path_reference(reference) {
            let referenced = std::fs::canonicalize(reference).map_err(|error| {
                format!("referenced file '{reference}' cannot be resolved: {error}")
            })?;
            let snapshot = std::fs::canonicalize(&row.snapshot).map_err(|error| {
                format!(
                    "snapshot {} for reference '{reference}' cannot be resolved: {error}",
                    row.snapshot.display()
                )
            })?;
            if referenced != snapshot {
                return Err(format!(
                    "reference '{reference}' is a local absolute path but the manifest \
                     snapshot {} is a different file; local file references must hash the \
                     referenced file itself",
                    row.snapshot.display()
                ));
            }
            SNAPSHOT_VERIFICATION_REFERENCED_FILE
        } else {
            // URL / GitHub shorthand / repo-relative spec path: the mapping
            // to a local archived snapshot is operator-attested. This seam
            // verifies the snapshot's bytes against the expected sha256 —
            // it is NOT live upstream verification, and no network fetch
            // ever happens here.
            SNAPSHOT_VERIFICATION_OPERATOR_ATTESTED
        };
        let mut file = open_snapshot_file(&row.snapshot)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; SNAPSHOT_HASH_BUFFER_BYTES];
        loop {
            let read = file.read(&mut buffer).map_err(|error| {
                format!("cannot hash snapshot {}: {error}", row.snapshot.display())
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let actual = format!("{:x}", hasher.finalize());
        if actual != row.sha256 {
            return Err(format!(
                "snapshot {} for reference '{reference}' hashes to {actual} but the \
                 manifest expects {}; snapshot drift or a wrong expected hash refuses \
                 the review",
                row.snapshot.display(),
                row.sha256
            ));
        }
        records.push(VerifiedSourceRecord {
            target_ref: reference.clone(),
            snapshot: row.snapshot.clone(),
            sha256: actual,
            verification,
        });
    }
    Ok(records)
}

/// Bundle hash derives from the canonical ref + verified-snapshot digest
/// records — never from a hash supplied by the caller.
fn derive_source_bundle_hash(records: &[VerifiedSourceRecord]) -> String {
    let mut ordered = records.to_vec();
    ordered.sort_by(|a, b| a.target_ref.cmp(&b.target_ref));
    let canonical = serde_json::to_string(&Value::Array(
        ordered
            .iter()
            .map(VerifiedSourceRecord::digest_json)
            .collect(),
    ))
    .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(WIKI_SOURCE_BUNDLE_DOMAIN.as_bytes());
    hasher.update([0]);
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

// ─── review candidate ───────────────────────────────────────────────────────

pub(crate) struct WikiReviewCandidate {
    references: Vec<String>,
    effective: crate::tool_params::EffectiveKnowledgeArtifactV1,
}

fn knowledge_artifact_entry(entry: &MemoryEntry) -> bool {
    let path = entry.path.as_str();
    path == "/wiki"
        || path.starts_with("/wiki/")
        || path == "/guide"
        || path.starts_with("/guide/")
        || entry.is_wiki()
        || matches!(
            entry.category.trim().to_ascii_lowercase().as_str(),
            "wiki" | "guide"
        )
}

fn review_candidate(entry: &MemoryEntry) -> Result<WikiReviewCandidate, String> {
    if memcore::is_reserved_wiki_rem_id(&entry.id) {
        return Err(format!(
            "'{}' is a reserved Wiki REM operation row, not a reviewable entry",
            entry.id
        ));
    }
    if !knowledge_artifact_entry(entry) {
        return Err(format!(
            "entry '{}' at path '{}' is not a Wiki/guide knowledge artifact",
            entry.id, entry.path
        ));
    }
    let references = preferred_wiki_references(&entry.metadata);
    let effective = derive_effective_knowledge_artifact(&entry.metadata, &entry.path, &entry.scope);
    Ok(WikiReviewCandidate {
        references,
        effective,
    })
}

/// The canonical review digest. Binds the target library identity, the
/// entry's id/revision/text/summary/scope/path, its effective
/// applicability, its canonical references, and the verified source
/// bundle hash. serde_json (without `preserve_order`) serializes object
/// keys in sorted order, so this JSON encoding is deterministic.
fn compute_review_digest(
    project: &str,
    store_db_path: &str,
    entry: &MemoryEntry,
    candidate: &WikiReviewCandidate,
    bundle_hash: &str,
) -> String {
    let effective = &candidate.effective;
    let binding = json!({
        "applicability": {
            "applies_to": effective.applies_to,
            "applicability_status": effective.applicability_status.as_str(),
            "known_exceptions": effective.known_exceptions,
            "knowledge_scope": effective.knowledge_scope.as_str(),
            "origin_projects": effective.origin_projects,
        },
        "bundle_hash": bundle_hash,
        "entry": {
            "id": entry.id,
            "path": entry.path,
            "revision": entry.revision,
            "scope": entry.scope,
            "summary": entry.summary,
            "text": entry.text,
        },
        "project": project,
        "references": candidate.references,
        "store_db_path": store_db_path,
    });
    let canonical = serde_json::to_string(&binding).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(WIKI_REVIEW_DIGEST_DOMAIN.as_bytes());
    hasher.update([0]);
    hasher.update(canonical.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn canonical_store_db_path(resolved: &Path) -> String {
    std::fs::canonicalize(resolved)
        .unwrap_or_else(|_| resolved.to_path_buf())
        .display()
        .to_string()
}

/// Approved metadata for the apply write: lifecycle/status active, review
/// approved, authority advisory (never playbook), with the bound receipt
/// nested in `review_receipt`. Everything else on the row — body, summary,
/// references, unrelated metadata — is carried through untouched.
fn approved_metadata(
    entry: &MemoryEntry,
    approver: &str,
    decided_at: &str,
    review_digest: &str,
    bundle_hash: &str,
    expected_revision: i64,
    store_ref: &str,
) -> Result<Value, String> {
    if !entry.metadata.is_object() {
        return Err(format!(
            "entry '{}' carries non-object metadata; refusing to approve",
            entry.id
        ));
    }
    let mut metadata = entry.metadata.clone();
    let object = metadata.as_object_mut().expect("object checked above");
    object.insert("lifecycle".to_string(), json!("active"));
    object.insert("status".to_string(), json!("active"));
    object.insert("review_status".to_string(), json!("approved"));
    // An approval never promotes a row to playbook authority.
    object.insert("authority".to_string(), json!("advisory"));
    object.insert(
        "source_bundle_hash".to_string(),
        json!(bundle_hash.to_string()),
    );
    object.insert(
        "review_receipt".to_string(),
        json!({
            "approver": approver,
            "decision": "approved",
            "decided_at": decided_at,
            "review_digest": review_digest,
            "source_bundle_hash": bundle_hash,
            "expected_revision": expected_revision,
            "store": store_ref,
        }),
    );
    Ok(metadata)
}

// ─── preview ────────────────────────────────────────────────────────────────

/// Read-only preview. Resolves the named Wiki store without creating or
/// migrating anything (a missing store is a refusal, an older-schema
/// store fails the read-only open), rereads the entry, verifies the
/// source manifest against the entry's references and the local snapshot
/// bytes, and reports the exact `expected_revision` + `review_digest` an
/// apply would have to bind.
pub(crate) fn preview_wiki_review(
    project: &str,
    id: &str,
    manifest_path: &Path,
) -> Result<Value, String> {
    let db_path = MemoryServer::resolve_named_project_db_path_in_home(
        project,
        &crate::path_utils::tachi_home(),
    )?;
    let db_str = db_path
        .to_str()
        .ok_or_else(|| format!("wiki store path is not valid UTF-8: {}", db_path.display()))?;
    let store = MemoryStore::open_read_only(db_str)
        .map_err(|error| format!("open wiki store {} read-only: {error}", db_path.display()))?;
    let entry = store
        .get_with_options(id, true)
        .map_err(|error| format!("read entry '{id}': {error}"))?
        .ok_or_else(|| format!("entry '{id}' not found in wiki store {project}"))?;
    if entry.archived {
        return Err(format!(
            "entry '{id}' is archived; archived rows cannot be approved"
        ));
    }
    let superseded_by: Option<String> = store
        .connection()
        .query_row(
            "SELECT superseded_by FROM memories WHERE id = ?1",
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(|error| format!("read superseded state for '{id}': {error}"))?
        .flatten();
    if let Some(superseded_by) = superseded_by {
        return Err(format!(
            "entry '{id}' is superseded by '{superseded_by}'; superseded rows cannot be approved"
        ));
    }

    let manifest = load_source_manifest(manifest_path)?;
    let candidate = review_candidate(&entry)?;
    let records = verify_source_manifest(&manifest, &candidate.references)?;
    let bundle_hash = derive_source_bundle_hash(&records);
    let store_db_path = canonical_store_db_path(&db_path);
    let review_digest =
        compute_review_digest(project, &store_db_path, &entry, &candidate, &bundle_hash);
    let effective = &candidate.effective;

    Ok(json!({
        "mode": "preview",
        "read_only": true,
        "project": project,
        "store_db_path": store_db_path,
        "entry": {
            "id": entry.id,
            "path": entry.path,
            "topic": entry.topic,
            "revision": entry.revision,
            "scope": entry.scope,
            "summary": entry.summary,
            "text": entry.text,
            "lifecycle": effective.lifecycle.as_str(),
            "authority": effective.authority.as_str(),
            "artifact_kind": effective.artifact_kind.as_str(),
        },
        "applicability": {
            "knowledge_scope": effective.knowledge_scope.as_str(),
            "origin_projects": effective.origin_projects,
            "applies_to": effective.applies_to,
            "applicability_status": effective.applicability_status.as_str(),
            "known_exceptions": effective.known_exceptions,
        },
        "references": records.iter().map(VerifiedSourceRecord::preview_json).collect::<Vec<_>>(),
        "source_bundle_hash": bundle_hash,
        "review_digest": review_digest,
        "apply": {
            "flags": ["--apply", "--approver", "--expected-revision", "--review-digest"],
            "expected_revision": entry.revision,
            "review_digest": review_digest,
        },
        "notes": [
            "URL/GitHub references map to operator-attested local snapshots: snapshot bytes \
             are verified against the manifest sha256, not against live upstream content; \
             no network fetch happens",
            "local absolute-path references must hash the referenced file itself",
            "validation observes snapshot bytes at validation time; no atomicity across \
             the manifest, the snapshots, and the database write is claimed",
        ],
    }))
}

// ─── apply ──────────────────────────────────────────────────────────────────

pub(crate) struct WikiReviewApply {
    pub(crate) project: String,
    pub(crate) id: String,
    pub(crate) approver: String,
    pub(crate) expected_revision: i64,
    pub(crate) expected_review_digest: String,
    pub(crate) manifest_path: PathBuf,
}

pub(crate) fn apply_wiki_review(
    server: &MemoryServer,
    request: &WikiReviewApply,
) -> Result<Value, String> {
    let approver = request.approver.trim();
    if approver.is_empty() {
        return Err("apply requires a nonempty --approver".to_string());
    }
    let expected_review_digest = normalize_expected_sha256(&request.expected_review_digest)
        .map_err(|error| format!("--review-digest: {error}"))?;
    let expected_revision = request.expected_revision;
    if expected_revision < 1 {
        return Err("--expected-revision must be a positive entry revision".to_string());
    }

    // Resolve the store identity before any mutation path: a missing
    // named store refuses here without creating anything.
    let db_path = server.resolve_server_named_project_db_path(&request.project)?;
    let store_db_path = canonical_store_db_path(&db_path);
    let store_ref = format!("named:{}", request.project);

    // Validate the manifest and snapshot bytes BEFORE the write path.
    let manifest = load_source_manifest(&request.manifest_path)?;

    let decided_at = Utc::now().to_rfc3339();
    let mut approved_receipt = None;

    server.with_named_project_store_identity_checked(&request.project, |store| {
        // Reread the row inside the identity-checked closure: checks
        // happen before any mutation.
        let entry = store
            .get_with_options(&request.id, true)
            .map_err(|error| format!("reread entry '{}': {error}", request.id))?
            .ok_or_else(|| {
                format!(
                    "entry '{}' not found in wiki store {}",
                    request.id, request.project
                )
            })?;
        if entry.archived {
            return Err(format!(
                "entry '{}' is archived; archived rows cannot be approved",
                request.id
            ));
        }
        let superseded_by: Option<String> = store
            .connection()
            .query_row(
                "SELECT superseded_by FROM memories WHERE id = ?1",
                params![request.id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|error| format!("read superseded state for '{}': {error}", request.id))?
            .flatten();
        if let Some(superseded_by) = superseded_by {
            return Err(format!(
                "entry '{}' is superseded by '{superseded_by}'; superseded rows cannot be \
                     approved",
                request.id
            ));
        }

        // Revision and digest must both still bind the previewed state.
        if entry.revision != expected_revision {
            return Err(format!(
                "stale revision for entry '{}': apply expected {expected_revision}, found \
                     {}; re-run the preview",
                request.id, entry.revision
            ));
        }
        let candidate = review_candidate(&entry)?;
        let records = verify_source_manifest(&manifest, &candidate.references)?;
        let bundle_hash = derive_source_bundle_hash(&records);
        let review_digest = compute_review_digest(
            &request.project,
            &store_db_path,
            &entry,
            &candidate,
            &bundle_hash,
        );
        if review_digest != expected_review_digest {
            return Err(format!(
                "review digest mismatch for entry '{}': the stored row, its references, its \
                     applicability, the manifest snapshots, or the store identity changed since \
                     the preview; re-run the preview and bind the new digest",
                request.id
            ));
        }

        let metadata = approved_metadata(
            &entry,
            approver,
            &decided_at,
            &review_digest,
            &bundle_hash,
            expected_revision,
            &store_ref,
        )?;

        // Effective-applicability gate, evaluated on the exact bytes
        // that would land: an approval whose derived lifecycle is not
        // `active` (e.g. shared scope without bounded applicability)
        // never reaches default read/search and is refused before any
        // mutation.
        let effective_after =
            derive_effective_knowledge_artifact(&metadata, &entry.path, &entry.scope);
        if effective_after.lifecycle != WikiLifecycleV1::Active {
            return Err(format!(
                "approved metadata would not be default-retrievable: derived lifecycle '{}' \
                     with issues {:?}; shared-scope knowledge needs bounded applicability and \
                     typed origins before it can go active",
                effective_after.lifecycle.as_str(),
                effective_after.validation_issues
            ));
        }

        // Verified write: complete-state CAS under BEGIN IMMEDIATE —
        // revision increments, body/summary/source stay byte-identical,
        // and a concurrent writer that touched the row first makes
        // this a no-op refusal instead of a lost approval.
        let expected_state = ExpectedMemoryState::from_entry(&entry, None);
        let updated = store
            .update_with_revision_if_expected_state(
                &entry.id,
                &entry.text,
                &entry.summary,
                &entry.source,
                &metadata,
                None,
                &expected_state,
            )
            .map_err(|error| format!("approved write for '{}': {error}", request.id))?;
        if !updated {
            return Err(format!(
                "entry '{}' changed inside the approval transaction (complete-state CAS \
                     mismatch); nothing was written — re-run the preview",
                request.id
            ));
        }

        approved_receipt = Some(json!({
            "mode": "apply",
            "approved": true,
            "project": request.project,
            "store_db_path": store_db_path,
            "id": entry.id,
            "path": entry.path,
            "approver": approver,
            "decided_at": decided_at,
            "previous_revision": entry.revision,
            "revision": entry.revision + 1,
            "lifecycle": "active",
            "authority": "advisory",
            "review_digest": review_digest,
            "source_bundle_hash": bundle_hash,
        }));
        Ok(())
    })?;

    let mut receipt = approved_receipt
        .ok_or_else(|| "approval transaction completed without a receipt".to_string())?;

    // Cache generation invalidation: same write-side fence the save path
    // uses, so no cached recall result can mask the newly active row.
    let recall_fence =
        crate::memory_search_ops::invalidate_recall_cache_after_write(server, "wiki_review");
    append_wiki_log(
        server,
        "review",
        &format!(
            "{} | {} | approved by {} | digest {}",
            request.id, request.project, approver, expected_review_digest
        ),
    );
    if let Some(object) = receipt.as_object_mut() {
        object.insert("recall_fence".to_string(), json!(recall_fence));
    }
    Ok(receipt)
}

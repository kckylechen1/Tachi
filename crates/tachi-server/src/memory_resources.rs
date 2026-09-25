//! Query-issued, version-bound MCP ResourceLinks for ordinary project memory.
//!
//! Resource URIs are comparison locators only. Every read resolves the current
//! request binding, rechecks its physical store identity and policy posture,
//! then returns the body from one current row only when revision and digest
//! still match.

use crate::tool_params::{SearchMemoryParams, TachiSearchParams};
use crate::MemoryServer;
use memcore::{MemoryEntry, Surface};
use rmcp::model::{Resource, ResourceContents};
use sha2::{Digest, Sha256};
use std::path::Path;

const URI_PREFIX: &str = "tachi-memory://v1/";
const MAX_RESOURCE_URI_BYTES: usize = 2048;
const MAX_ENCODED_ID_BYTES: usize = 1536;
const MAX_ID_BYTES: usize = 512;
const SOURCE_DOMAIN: &[u8] = b"tachi.memory.resource.source.v1\0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedResourceUri {
    pub(crate) source_fingerprint: String,
    pub(crate) id: String,
    pub(crate) revision: i64,
    pub(crate) body_sha256: String,
}

pub(crate) fn resource_link_for_entry(
    source_fingerprint: &str,
    entry: &MemoryEntry,
) -> Option<Resource> {
    if !is_ordinary_active_entry(entry)
        || !is_lower_sha256(source_fingerprint)
        || entry.id.is_empty()
        || entry.id.len() > MAX_ID_BYTES
    {
        return None;
    }
    let encoded_id = encode_id(&entry.id);
    if encoded_id.len() > MAX_ENCODED_ID_BYTES {
        return None;
    }
    let uri = format!(
        "{URI_PREFIX}{}/{}/{}/{}",
        source_fingerprint,
        encoded_id,
        entry.revision,
        sha256_hex(entry.text.as_bytes()),
    );
    (uri.len() <= MAX_RESOURCE_URI_BYTES).then(|| {
        Resource::new(uri, "memory")
            .with_description("Current memory text")
            .with_mime_type("text/plain")
    })
}

pub(crate) fn eligible_search_entry(entry: &MemoryEntry, params: &SearchMemoryParams) -> bool {
    let constrained_role = params
        .agent_role
        .as_deref()
        .is_some_and(|role| !role.trim().is_empty());
    !constrained_role
        && !params.include_archived
        && !params.include_training
        && params.as_of.is_none()
        && is_ordinary_active_entry(entry)
}

fn is_ordinary_active_entry(entry: &MemoryEntry) -> bool {
    entry.revision > 0
        && !entry.archived
        && entry.valid_until.is_none()
        && matches!(entry.scope.as_str(), "user" | "project" | "general")
        && memcore::surface_of(entry) == Surface::Memory
        && !memcore::is_internal_only_row(entry)
        && !memcore::is_namespace_search_noise(entry, None)
        && !memcore::is_eval_entry(entry)
}

pub(crate) fn parse_resource_uri(uri: &str) -> Option<ParsedResourceUri> {
    if uri.len() > MAX_RESOURCE_URI_BYTES || !uri.starts_with(URI_PREFIX) {
        return None;
    }
    let mut segments = uri[URI_PREFIX.len()..].split('/');
    let source_fingerprint = segments.next()?;
    let encoded_id = segments.next()?;
    let revision_text = segments.next()?;
    let body_sha256 = segments.next()?;
    if segments.next().is_some()
        || !is_lower_sha256(source_fingerprint)
        || !is_lower_sha256(body_sha256)
        || encoded_id.is_empty()
        || encoded_id.len() > MAX_ENCODED_ID_BYTES
    {
        return None;
    }
    let id = decode_canonical_id(encoded_id)?;
    if id.is_empty() || id.len() > MAX_ID_BYTES {
        return None;
    }
    if revision_text.is_empty()
        || !revision_text.bytes().all(|byte| byte.is_ascii_digit())
        || revision_text.starts_with('0')
    {
        return None;
    }
    let revision = revision_text.parse::<i64>().ok()?;
    if revision <= 0 {
        return None;
    }
    Some(ParsedResourceUri {
        source_fingerprint: source_fingerprint.to_string(),
        id,
        revision,
        body_sha256: body_sha256.to_string(),
    })
}

fn decode_canonical_id(encoded: &str) -> Option<String> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hi = *bytes.get(index + 1)?;
                let lo = *bytes.get(index + 2)?;
                decoded.push((hex_nibble(hi)? << 4) | hex_nibble(lo)?);
                index += 3;
            }
            byte if is_safe_id_byte(byte) => {
                decoded.push(byte);
                index += 1;
            }
            _ => return None,
        }
    }
    let id = String::from_utf8(decoded).ok()?;
    (encode_id(&id) == encoded).then_some(id)
}

fn encode_id(id: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(id.len());
    for byte in id.bytes() {
        if is_safe_id_byte(byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(byte >> 4) as usize]));
            encoded.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    encoded
}

fn is_safe_id_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'~')
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_lower_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// Return a domain-separated identity only for stable file identities. On
/// platforms where the existing opened-handle verifier can only supply a path,
/// Resources remain unavailable rather than treating that path as identity.
pub(crate) fn source_fingerprint(path: &Path, physical_identity: &str) -> Option<String> {
    if !(physical_identity.starts_with("unix:") || physical_identity.starts_with("windows:")) {
        return None;
    }
    let canonical_path = std::fs::canonicalize(path).ok()?;
    let path_bytes = canonical_path.as_os_str().as_encoded_bytes();
    let identity_bytes = physical_identity.as_bytes();
    let mut hasher = Sha256::new();
    hasher.update(SOURCE_DOMAIN);
    hasher.update((path_bytes.len() as u64).to_be_bytes());
    hasher.update(path_bytes);
    hasher.update((identity_bytes.len() as u64).to_be_bytes());
    hasher.update(identity_bytes);
    Some(hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[(byte >> 4) as usize]));
        output.push(char::from(HEX[(byte & 0x0f) as usize]));
    }
    output
}

pub(crate) fn resource_issuance_allowed(
    server: &MemoryServer,
    params: &TachiSearchParams,
    bound_project: Option<&str>,
) -> bool {
    let Some(bound_project) = bound_project.filter(|project| !project.trim().is_empty()) else {
        return false;
    };
    if params.project.as_deref() != Some(bound_project)
        || !matches!(params.scope.to_ascii_lowercase().as_str(), "memory" | "all")
        || params
            .agent_role
            .as_deref()
            .is_some_and(|role| !role.trim().is_empty())
        || params.include_archived
        || params.include_training
        || params.as_of.is_some()
    {
        return false;
    }
    sandbox_policy_clear(server).unwrap_or(false)
}

pub(crate) fn sandbox_policy_clear(server: &MemoryServer) -> Result<bool, String> {
    server.with_global_store_read(|store| {
        store
            .has_configured_sandbox_rules()
            .map(|configured| !configured)
            .map_err(|error| error.to_string())
    })
}

/// A store's persisted role is not its current project alias. Bind Resources
/// to the resolved project file and its live handle, including cached handles
/// whose role predates named-project admission. Global and Wiki stores remain
/// outside this feature even if a project alias points at them.
pub(crate) fn verified_project_source(
    server: &MemoryServer,
    project_name: &str,
    store: &memcore::MemoryStore,
) -> Option<String> {
    if store.db_label() == "global" || store.is_wiki_corpus_store() {
        return None;
    }
    let path = server
        .resolve_server_named_project_db_path(project_name)
        .ok()
        .and_then(|path| std::fs::canonicalize(path).ok())?;
    let global = std::fs::canonicalize(server.global_db_path_buf()).ok()?;
    if crate::physical_db_identity::same_physical_file(&path, &global) {
        return None;
    }
    store.verify_opened_physical_db_identity(&path).ok()?;
    let identity = store.opened_physical_db_identity()?;
    let fingerprint = source_fingerprint(&path, identity)?;
    store.verify_opened_physical_db_identity(&path).ok()?;
    Some(fingerprint)
}

pub(crate) fn read_resource_text(
    server: &MemoryServer,
    project_name: &str,
    reference: &ParsedResourceUri,
) -> Result<String, ()> {
    if !sandbox_policy_clear(server).map_err(|_| ())? {
        return Err(());
    }
    let body = server
        .with_named_project_store_read_identity_checked(project_name, |store| {
            let source_fingerprint = verified_project_source(server, project_name, store)
                .ok_or_else(|| "resource unavailable".to_string())?;
            if source_fingerprint != reference.source_fingerprint {
                return Err("resource unavailable".to_string());
            }
            let entry = store
                .get_active_resource_entry(&reference.id)
                .map_err(|_| "resource unavailable".to_string())?
                .filter(is_ordinary_active_entry)
                .ok_or_else(|| "resource unavailable".to_string())?;
            if entry.revision != reference.revision
                || sha256_hex(entry.text.as_bytes()) != reference.body_sha256
            {
                return Err("resource unavailable".to_string());
            }
            Ok(entry.text)
        })
        .map_err(|_| ())?;
    if !sandbox_policy_clear(server).map_err(|_| ())? {
        return Err(());
    }
    Ok(body)
}

pub(crate) fn read_response(
    uri: String,
    text: String,
    modern: bool,
) -> rmcp::model::ReadResourceResult {
    let mut result = rmcp::model::ReadResourceResult::new(vec![ResourceContents::text(text, uri)]);
    if modern {
        result.ttl_ms = Some(0);
        result.cache_scope = Some(rmcp::model::CacheScope::Private);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use memcore::{MemoryEntry, MemoryStore};
    use std::path::Path;

    fn entry(id: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            path: "/memory/notes".to_string(),
            summary: "summary".to_string(),
            text: "exact body".to_string(),
            importance: 0.5,
            timestamp: "2026-09-25T00:00:00Z".to_string(),
            valid_from: "2026-09-25T00:00:00Z".to_string(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: vec![],
            persons: vec![],
            entities: vec![],
            location: String::new(),
            source: "manual".to_string(),
            scope: "project".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 4,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    #[test]
    fn resource_uri_round_trips_canonical_encoded_ids() {
        let id = "id/with space.雪";
        assert_eq!(encode_id(id), "id%2Fwith%20space%2E%E9%9B%AA");
        let link = resource_link_for_entry(&"a".repeat(64), &entry(id)).expect("resource link");
        let parsed = parse_resource_uri(&link.uri).expect("canonical URI");
        assert_eq!(parsed.id, id);
        assert_eq!(parsed.revision, 4);
        assert_eq!(parsed.body_sha256, sha256_hex(b"exact body"));
    }

    #[test]
    fn malformed_or_noncanonical_resource_uris_are_rejected() {
        let digest = "b".repeat(64);
        for uri in [
            format!("{URI_PREFIX}{}//4/{digest}", "a".repeat(64)),
            format!("{URI_PREFIX}{}/{}/04/{digest}", "a".repeat(64), "id"),
            format!("{URI_PREFIX}{}/{}/0/{digest}", "a".repeat(64), "id"),
            format!("{URI_PREFIX}{}/{}/4/{digest}?x=1", "a".repeat(64), "id"),
            format!("{URI_PREFIX}{}/%2f/4/{digest}", "a".repeat(64)),
        ] {
            assert!(parse_resource_uri(&uri).is_none(), "accepted {uri}");
        }
    }

    fn project_fixture(
        project_name: &str,
        fixture_entry: MemoryEntry,
    ) -> (
        crate::tests::TestServer,
        std::path::PathBuf,
        ParsedResourceUri,
    ) {
        let (server, project_db) = crate::tests::make_server_with_project_fixture(project_name);
        server
            .with_named_project_store(project_name, |store| {
                store
                    .upsert(&fixture_entry)
                    .map_err(|error| format!("seed resource fixture: {error}"))
            })
            .expect("seed named project resource fixture");
        let link = server
            .with_named_project_store_read_identity_checked(project_name, |store| {
                let path = std::fs::canonicalize(&project_db)
                    .map_err(|error| format!("canonicalize resource fixture: {error}"))?;
                store
                    .verify_opened_physical_db_identity(&path)
                    .map_err(|error| format!("verify resource fixture identity: {error}"))?;
                let identity = store
                    .opened_physical_db_identity()
                    .ok_or_else(|| "resource fixture lacks physical identity".to_string())?;
                let fingerprint = source_fingerprint(&path, identity)
                    .ok_or_else(|| "resource fixture fingerprint unavailable".to_string())?;
                resource_link_for_entry(&fingerprint, &fixture_entry)
                    .ok_or_else(|| "resource fixture is not eligible".to_string())
            })
            .expect("create link from exact named project store");
        let reference = parse_resource_uri(&link.uri).expect("issued URI parses");
        (server, project_db, reference)
    }

    fn link_for_open_store(
        store: &MemoryStore,
        db_path: &Path,
        fixture_entry: &MemoryEntry,
    ) -> rmcp::model::Resource {
        let canonical = std::fs::canonicalize(db_path).expect("canonical store path");
        store
            .verify_opened_physical_db_identity(&canonical)
            .expect("fixture store identity");
        let identity = store
            .opened_physical_db_identity()
            .expect("fixture physical identity");
        let fingerprint = source_fingerprint(&canonical, identity).expect("store fingerprint");
        resource_link_for_entry(&fingerprint, fixture_entry).expect("eligible fixture link")
    }

    fn reference_for_stored_entry(
        server: &crate::tests::TestServer,
        project_name: &str,
        project_db: &Path,
        fixture_entry: &MemoryEntry,
    ) -> ParsedResourceUri {
        server
            .with_named_project_store_read_identity_checked(project_name, |store| {
                let path = std::fs::canonicalize(project_db)
                    .map_err(|error| format!("canonicalize stored fixture: {error}"))?;
                let identity = store
                    .opened_physical_db_identity()
                    .ok_or_else(|| "stored fixture lacks physical identity".to_string())?;
                let source_fingerprint = source_fingerprint(&path, identity)
                    .ok_or_else(|| "stored fixture fingerprint unavailable".to_string())?;
                Ok(ParsedResourceUri {
                    source_fingerprint,
                    id: fixture_entry.id.clone(),
                    revision: fixture_entry.revision,
                    body_sha256: sha256_hex(fixture_entry.text.as_bytes()),
                })
            })
            .expect("construct test reference from the current physical store")
    }

    #[test]
    fn issuance_respects_canonical_id_limits_and_allows_the_exact_boundary() {
        let source = "a".repeat(64);
        let max_ascii_id = "i".repeat(MAX_ID_BYTES);
        let max_link = resource_link_for_entry(&source, &entry(&max_ascii_id))
            .expect("512-byte IDs remain readable");
        assert_eq!(
            parse_resource_uri(&max_link.uri)
                .expect("boundary URI parses")
                .id,
            max_ascii_id
        );

        let mut too_long = entry(&"i".repeat(MAX_ID_BYTES + 1));
        assert!(resource_link_for_entry(&source, &too_long).is_none());

        let max_encoded_id = "é".repeat(MAX_ID_BYTES / 2);
        assert_eq!(encode_id(&max_encoded_id).len(), MAX_ENCODED_ID_BYTES);
        let encoded_boundary_link = resource_link_for_entry(&source, &entry(&max_encoded_id))
            .expect("maximum encoded ID remains readable");
        assert_eq!(
            parse_resource_uri(&encoded_boundary_link.uri)
                .expect("encoded boundary URI parses")
                .id,
            max_encoded_id
        );

        too_long.id.clear();
        assert!(resource_link_for_entry(&source, &too_long).is_none());
    }

    #[test]
    fn ordinary_entry_issuance_rejects_inactive_private_internal_and_docs_rows() {
        let source = "a".repeat(64);
        for ordinary_scope in ["user", "project", "general"] {
            let mut candidate = entry("ordinary-scope-fixture");
            candidate.scope = ordinary_scope.to_string();
            assert!(
                resource_link_for_entry(&source, &candidate).is_some(),
                "ordinary persisted scope {ordinary_scope} should remain eligible"
            );
        }

        type EntryMutation = fn(&mut MemoryEntry);
        let cases: [(&str, EntryMutation); 7] = [
            ("archived", |entry: &mut MemoryEntry| entry.archived = true),
            ("expired", |entry: &mut MemoryEntry| {
                entry.valid_until = Some("2026-09-25T00:00:00Z".to_string())
            }),
            ("private scope", |entry: &mut MemoryEntry| {
                entry.scope = "private".to_string()
            }),
            ("global routing scope", |entry: &mut MemoryEntry| {
                entry.scope = "global".to_string()
            }),
            ("internal row", |entry: &mut MemoryEntry| {
                entry.source = "foundry_recall_rerank_cache".to_string()
            }),
            ("reserved internal ID", |entry: &mut MemoryEntry| {
                entry.id = "wiki-rem:internal-fixture".to_string()
            }),
            ("docs row", |entry: &mut MemoryEntry| {
                entry.category = "guide".to_string()
            }),
        ];
        for (label, mutate) in cases {
            let mut candidate = entry("ordinary-fixture");
            mutate(&mut candidate);
            assert!(
                resource_link_for_entry(&source, &candidate).is_none(),
                "issued a link for {label}"
            );
        }

        let role_constrained: SearchMemoryParams = serde_json::from_value(serde_json::json!({
            "query": "ordinary-fixture",
            "agent_role": "code-review"
        }))
        .expect("role-constrained search params");
        assert!(
            !eligible_search_entry(&entry("ordinary-fixture"), &role_constrained),
            "role-filtered search cannot issue a ResourceLink"
        );
    }

    #[test]
    fn resource_read_rejects_an_internal_row_even_with_matching_source_revision_and_digest() {
        // Typed writes normalize the closed MemoryScope enum; a "private"
        // candidate is rejected at issuance in the classifier test above. The
        // sealed private database itself is tested through the HTTP handler in
        // rmcp_compat/resources.rs.
        let internal_project = "resource-internal-row";
        let mut internal_entry = entry("resource-internal-row-id");
        internal_entry.source = "foundry_recall_rerank_cache".to_string();
        let (internal_server, internal_db) =
            crate::tests::make_server_with_project_fixture(internal_project);
        internal_server
            .with_named_project_store(internal_project, |store| {
                store
                    .upsert(&internal_entry)
                    .map_err(|error| format!("seed internal resource fixture: {error}"))
            })
            .expect("seed internal row");
        let internal_reference = reference_for_stored_entry(
            &internal_server,
            internal_project,
            &internal_db,
            &internal_entry,
        );
        assert!(
            read_resource_text(&internal_server, internal_project, &internal_reference).is_err(),
            "an internal row cannot be read even with matching reference fields"
        );
    }

    #[test]
    fn reads_exact_body_and_rejects_revision_or_digest_mismatches() {
        let project_name = "resource-current-body";
        let fixture_entry = entry("resource-current-body-id");
        let expected_text = fixture_entry.text.clone();
        let (server, project_db, reference) = project_fixture(project_name, fixture_entry.clone());
        assert_eq!(
            read_resource_text(&server, project_name, &reference).expect("current body"),
            expected_text
        );

        let mut digest_mismatch = reference.clone();
        digest_mismatch.body_sha256 = sha256_hex(b"different exact body");
        assert!(
            read_resource_text(&server, project_name, &digest_mismatch).is_err(),
            "a current revision with a stale digest must fail closed"
        );

        let mut revision_mismatch = reference.clone();
        revision_mismatch.revision += 1;
        assert!(
            read_resource_text(&server, project_name, &revision_mismatch).is_err(),
            "a stale revision must fail closed"
        );

        server
            .with_named_project_store(project_name, |store| {
                let updated = store
                    .update_with_revision(
                        &fixture_entry.id,
                        "updated exact body",
                        "updated summary",
                        "fixture",
                        &serde_json::json!({}),
                        None,
                        fixture_entry.revision,
                    )
                    .map_err(|error| format!("revisioned resource update: {error}"))?;
                if !updated {
                    return Err("fixture revision update did not apply".to_string());
                }
                Ok(())
            })
            .expect("update fixture through typed store API");
        assert!(
            read_resource_text(&server, project_name, &reference).is_err(),
            "old URI cannot return body after a revision change"
        );

        let current = server
            .with_named_project_store_read_identity_checked(project_name, |store| {
                store
                    .get_active_resource_entry(&fixture_entry.id)
                    .map_err(|error| format!("read updated fixture: {error}"))?
                    .ok_or_else(|| "updated fixture missing".to_string())
            })
            .expect("read current materialized row");
        let current_link = server
            .with_named_project_store_read_identity_checked(project_name, |store| {
                let canonical = std::fs::canonicalize(&project_db)
                    .map_err(|error| format!("canonicalize project DB: {error}"))?;
                let identity = store
                    .opened_physical_db_identity()
                    .ok_or_else(|| "current store physical identity unavailable".to_string())?;
                let fingerprint = source_fingerprint(&canonical, identity)
                    .ok_or_else(|| "current store fingerprint unavailable".to_string())?;
                resource_link_for_entry(&fingerprint, &current)
                    .ok_or_else(|| "updated entry no longer eligible".to_string())
            })
            .expect("link updated current row");
        let current_reference = parse_resource_uri(&current_link.uri).expect("current URI");
        assert_eq!(
            read_resource_text(&server, project_name, &current_reference)
                .expect("read updated exact body"),
            "updated exact body"
        );
    }

    #[test]
    fn archive_supersession_and_source_replacement_invalidate_references() {
        let (archived_server, _, archived_reference) =
            project_fixture("resource-archived", entry("resource-archived-id"));
        archived_server
            .with_named_project_store("resource-archived", |store| {
                store
                    .archive_memory("resource-archived-id")
                    .map(|_| ())
                    .map_err(|error| format!("archive fixture: {error}"))
            })
            .expect("archive through typed lifecycle API");
        assert!(
            read_resource_text(&archived_server, "resource-archived", &archived_reference).is_err()
        );

        let (superseded_server, _, superseded_reference) =
            project_fixture("resource-superseded", entry("resource-superseded-id"));
        superseded_server
            .with_named_project_store("resource-superseded", |store| {
                store
                    .supersede_memory("resource-superseded-id", "resource-successor")
                    .map(|_| ())
                    .map_err(|error| format!("supersede fixture: {error}"))
            })
            .expect("supersede through typed lifecycle API");
        assert!(read_resource_text(
            &superseded_server,
            "resource-superseded",
            &superseded_reference
        )
        .is_err());

        let replacement_project = "resource-replaced";
        let original = entry("resource-replaced-id");
        let (replacement_server, project_db, original_reference) =
            project_fixture(replacement_project, original.clone());
        assert_eq!(
            read_resource_text(
                &replacement_server,
                replacement_project,
                &original_reference
            )
            .expect("original body before replacement"),
            original.text
        );
        let replacement_path = project_db.with_extension("replacement.db");
        let replacement = original;
        let mut replacement_store = MemoryStore::open_with_label(
            replacement_path.to_str().expect("replacement DB path"),
            replacement_project,
        )
        .expect("create replacement source store");
        replacement_store
            .upsert(&replacement)
            .expect("seed replacement source row");
        drop(replacement_store);

        let manifest_path = replacement_server.tachi_home_dir().join("manifest.json");
        let mut manifest = crate::manifest::Manifest::load_or_empty(&manifest_path);
        let binding = manifest
            .dbs
            .iter_mut()
            .find(|db| db.scope_hint == format!("project:{replacement_project}"))
            .expect("project manifest binding");
        binding.path = replacement_path.display().to_string();
        manifest
            .save(&manifest_path)
            .expect("retarget project binding");
        assert!(
            read_resource_text(
                &replacement_server,
                replacement_project,
                &original_reference
            )
            .is_err(),
            "same-ID replacement source must not satisfy the old reference"
        );
    }

    #[test]
    fn duplicate_ids_in_global_and_other_project_cannot_fallback_into_bound_project() {
        let project_name = "resource-duplicate-a";
        let mut project_entry = entry("resource-duplicate-id");
        project_entry.text = "bound project A exact body".to_string();
        let (server_a, _, reference_a) = project_fixture(project_name, project_entry.clone());

        let mut global_entry = project_entry.clone();
        global_entry.text = "global duplicate must not be returned".to_string();
        server_a
            .with_global_store(|store| {
                store
                    .upsert(&global_entry)
                    .map_err(|error| format!("seed global duplicate: {error}"))
            })
            .expect("seed global duplicate ID");
        let global_link = server_a
            .with_global_store_read(|store| {
                Ok(link_for_open_store(
                    store,
                    &server_a.global_db_path_buf(),
                    &global_entry,
                ))
            })
            .expect("create deliberately global-addressed reference");
        let global_reference = parse_resource_uri(&global_link.uri).expect("global URI parses");

        let mut project_b_entry = project_entry.clone();
        project_b_entry.text = "other project duplicate must not be returned".to_string();
        let (_server_b, _project_db_b, reference_b) =
            project_fixture("resource-duplicate-b", project_b_entry);

        assert_eq!(
            read_resource_text(&server_a, project_name, &reference_a)
                .expect("bound project A body"),
            "bound project A exact body"
        );
        assert!(
            read_resource_text(&server_a, project_name, &global_reference).is_err(),
            "global URI cannot fall back to same-ID project content"
        );
        assert!(
            read_resource_text(&server_a, project_name, &reference_b).is_err(),
            "project B URI cannot fall back to same-ID project A content"
        );
    }

    #[test]
    fn sandbox_rule_insertion_revokes_existing_read_and_new_issuance() {
        let project_name = "resource-sandbox";
        let fixture_entry = entry("resource-sandbox-id");
        let (server, _, reference) = project_fixture(project_name, fixture_entry.clone());
        let search: TachiSearchParams = serde_json::from_value(serde_json::json!({
            "query": "resource-sandbox-id",
            "scope": "memory",
            "project": project_name
        }))
        .expect("ordinary project search params");
        assert!(resource_issuance_allowed(
            &server,
            &search,
            Some(project_name)
        ));
        assert!(read_resource_text(&server, project_name, &reference).is_ok());

        server
            .with_global_store(|store| {
                store
                    .set_sandbox_rule("code-review", "/memory/**", "deny")
                    .map_err(|error| format!("insert sandbox rule: {error}"))
            })
            .expect("insert configured sandbox rule");
        assert!(!resource_issuance_allowed(
            &server,
            &search,
            Some(project_name)
        ));
        assert!(
            read_resource_text(&server, project_name, &reference).is_err(),
            "a rule added after issuance revokes Resource reads"
        );

        let role_search: TachiSearchParams = serde_json::from_value(serde_json::json!({
            "query": "resource-sandbox-id",
            "scope": "memory",
            "project": project_name,
            "agent_role": "code-review"
        }))
        .expect("role-constrained project search params");
        assert!(!resource_issuance_allowed(
            &server,
            &role_search,
            Some(project_name)
        ));

        let global_db = server.global_db_path_buf();
        crate::test_support::with_unrestricted_fixture_connection(&global_db, |connection| {
            connection.execute_batch("DROP TABLE sandbox_rules")
        })
        .expect("make the isolated fixture policy table unreadable");
        assert!(!resource_issuance_allowed(
            &server,
            &search,
            Some(project_name)
        ));
        assert!(
            read_resource_text(&server, project_name, &reference).is_err(),
            "policy-read uncertainty must disable Resource reads"
        );
    }
}

use super::types::CredentialMaterializeStepReport;
use super::CREDENTIAL_MATERIALIZATION_NAMESPACE;
use memcore::anchored_fs::DirectoryIdentity;
use memcore::store::state::StateTransaction;
use memcore::MemoryStore;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tachi_params::util::stable_hash;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(super) struct ManagedCredentialMaterialization {
    pub(super) version: u32,
    pub(super) profile: String,
    pub(super) consumer: String,
    pub(super) materializer_type: String,
    pub(super) source: String,
    pub(super) resolved_secret: String,
    pub(super) target: String,
    pub(super) content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) run_root_canonical: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) run_root_dev: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) run_root_ino: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) target_rel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) restore_rel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) run_generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) target_dev: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) target_ino: Option<u64>,
    pub(super) chmod: Option<String>,
    pub(super) managed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cleanup_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cleaned_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) cleanup_run_dir: Option<String>,
    /// Terminal rows keep a 30-day audit tail. Active rows have no TTL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) expires_at: Option<String>,
}

pub(super) fn legacy_materialization_key(
    profile_name: &str,
    consumer: &str,
    materializer_type: &str,
    resolved_secret: &str,
    target: &str,
) -> String {
    format!(
        "managed:{}",
        stable_hash(&format!(
            "{profile_name}\u{1f}{consumer}\u{1f}{materializer_type}\u{1f}{resolved_secret}\u{1f}{target}"
        ))
    )
}

pub(super) fn managed_materialization_content_hash(
    profile_name: &str,
    consumer: &str,
    target: &str,
    content: &str,
) -> String {
    format!(
        "stable-fnv1a:{}",
        stable_hash(&format!(
            "{profile_name}\u{1f}{consumer}\u{1f}{target}\u{1f}{content}"
        ))
    )
}

impl ManagedCredentialMaterialization {
    pub(super) fn bound_target(&self) -> Option<(DirectoryIdentity, &Path)> {
        Some((
            DirectoryIdentity {
                device: self.run_root_dev?,
                inode: self.run_root_ino?,
            },
            Path::new(self.target_rel.as_deref()?),
        ))
    }

    pub(super) fn target_identity(&self) -> Option<(u64, u64)> {
        self.target_dev.zip(self.target_ino)
    }

    pub(super) fn is_legacy(&self) -> bool {
        self.version == 1 && self.run_root_dev.is_none()
    }

    #[cfg(unix)]
    pub(super) fn key(&self) -> String {
        let legacy = legacy_materialization_key(
            &self.profile,
            &self.consumer,
            &self.materializer_type,
            &self.resolved_secret,
            &self.target,
        );
        let Some((identity, _)) = self.bound_target() else {
            return legacy;
        };
        let identity_key = format!(
            "{legacy}:{}:{}:{:?}",
            identity.device, identity.inode, self.run_generation
        );
        format!("managed:v2:{}", stable_hash(&identity_key))
    }
}

#[cfg(unix)]
pub(super) fn prepare_managed_materialization(
    profile: &str,
    consumer: &str,
    step: &CredentialMaterializeStepReport,
    content: &str,
    root: &Path,
    root_id: DirectoryIdentity,
    generation: Option<&str>,
) -> ManagedCredentialMaterialization {
    let relative = Path::new(&step.target)
        .strip_prefix(root)
        .expect("planned file target is inside canonical run_dir");
    ManagedCredentialMaterialization {
        version: 2,
        profile: profile.to_owned(),
        consumer: consumer.to_owned(),
        materializer_type: step.materializer_type.clone(),
        source: step.source.clone(),
        resolved_secret: step.resolved_secret.clone(),
        target: step.target.clone(),
        content_hash: managed_materialization_content_hash(
            profile,
            consumer,
            &step.target,
            content,
        ),
        run_root_canonical: Some(root.to_string_lossy().into_owned()),
        run_root_dev: Some(root_id.device),
        run_root_ino: Some(root_id.inode),
        target_rel: Some(relative.to_string_lossy().into_owned()),
        restore_rel: None,
        run_generation: generation.map(str::to_owned),
        target_dev: None,
        target_ino: None,
        chmod: step.chmod.clone(),
        managed_at: jiff::Timestamp::now().to_string(),
        cleanup_status: None,
        cleaned_at: None,
        cleanup_run_dir: None,
        expires_at: None,
    }
}

#[cfg(unix)]
pub(super) fn supersede_colliding_materializations(
    state: &StateTransaction<'_>,
    incoming: &ManagedCredentialMaterialization,
    allow_existing: bool,
) -> Result<(), String> {
    for row in state
        .list_state(CREDENTIAL_MATERIALIZATION_NAMESPACE)
        .map_err(|err| format!("list credential materialization metadata: {err}"))?
    {
        if row.key.starts_with(PUBLICATION_PREFIX) {
            continue;
        }
        let mut existing: ManagedCredentialMaterialization = serde_json::from_str(&row.value_json)
            .map_err(|err| format!("parse credential materialization '{}': {err}", row.key))?;
        if existing.cleanup_status.is_some() {
            continue;
        }
        let collides = if existing.is_legacy() {
            let target = Path::new(&existing.target);
            target == Path::new(&incoming.target)
                || legacy_target_names_path(target, Path::new(&incoming.target))
        } else {
            existing.bound_target() == incoming.bound_target()
        };
        // Reopen a differently spelled binding after publication. Case and
        // nested-root aliases now reach the new inode, including rows written
        // before inode binding. A distinct hard-link name still reaches the
        // old inode and needs its own active cleanup record.
        let resolves_to_new_inode = !collides
            && bound_entry_identity(&existing, incoming, None)
                .is_some_and(|file| Some(file) == incoming.target_identity());
        if !collides && !resolves_to_new_inode {
            continue;
        }
        if existing.target_identity().is_some()
            && bound_entry_identity(
                &existing,
                incoming,
                Some(&super::cleanup::cleanup_capture_name(&row.key)),
            ) == existing.target_identity()
        {
            return Err(format!(
                "credential target '{}' has an interrupted cleanup; retry cleanup before replacement",
                existing.target
            ));
        }
        if !allow_existing {
            return Err(format!(
                "managed credential target '{}' already exists for profile '{}'; replacement requires allow_existing",
                incoming.target, existing.profile
            ));
        }
        let closed = close_metadata(state, &row, &mut existing, "superseded", None)?;
        if !closed {
            return Err(format!(
                "credential materialization '{}' changed before replacement",
                row.key
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn bound_entry_identity(
    metadata: &ManagedCredentialMaterialization,
    incoming: &ManagedCredentialMaterialization,
    sibling: Option<&str>,
) -> Option<(u64, u64)> {
    if sibling.is_none() && metadata.is_legacy() {
        let target = Path::new(&metadata.target);
        let parent = target
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let canonical = std::fs::canonicalize(parent).ok()?;
        let writer = super::safety::unix::RunWriter::open(&canonical).ok()?;
        return writer.existing_identity(Path::new(target.file_name()?));
    }

    let (identity, relative) = metadata.bound_target()?;
    // A renamed run root can still be identified among the target's
    // ancestors. Old v2 rows only recorded that directory's identity.
    let recorded = metadata.run_root_canonical.as_deref().map(Path::new);
    let ancestors = Path::new(&incoming.target).parent()?.ancestors();
    let writer = recorded.into_iter().chain(ancestors).find_map(|root| {
        super::safety::unix::RunWriter::open(root)
            .ok()
            .filter(|writer| writer.identity == identity)
    })?;
    match sibling {
        Some(name) => writer.existing_identity(&relative.with_file_name(name)),
        None => writer.existing_identity(relative),
    }
}

#[cfg(unix)]
fn legacy_target_names_path(target: &Path, incoming: &Path) -> bool {
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    target.file_name() == incoming.file_name()
        && std::fs::canonicalize(parent)
            .is_ok_and(|canonical| Some(canonical.as_path()) == incoming.parent())
}

pub(super) const PUBLICATION_PREFIX: &str = "publication:";

#[cfg(unix)]
pub(super) fn record_managed_materialization(
    state: &StateTransaction<'_>,
    key: &str,
    metadata: &ManagedCredentialMaterialization,
) -> Result<(), String> {
    let value = serde_json::to_string(metadata)
        .map_err(|err| format!("serialize credential materialization metadata: {err}"))?;
    state
        .set_state(CREDENTIAL_MATERIALIZATION_NAMESPACE, key, &value)
        .map_err(|err| format!("record credential materialization metadata: {err}"))?;
    Ok(())
}

pub(super) fn read_managed_materialization(
    store: &MemoryStore,
    profile_name: &str,
    consumer: &str,
    step: &CredentialMaterializeStepReport,
    binding: Option<(DirectoryIdentity, &Path)>,
) -> Result<Option<ManagedCredentialMaterialization>, String> {
    if let Some(binding) = binding {
        let mut terminal = None;
        for row in store
            .list_state(CREDENTIAL_MATERIALIZATION_NAMESPACE)
            .map_err(|err| format!("list credential materialization metadata: {err}"))?
        {
            if row.key.starts_with(PUBLICATION_PREFIX) {
                continue;
            }
            let Ok(metadata) =
                serde_json::from_str::<ManagedCredentialMaterialization>(&row.value_json)
            else {
                continue;
            };
            if metadata.profile == profile_name
                && metadata.consumer == consumer
                && metadata.materializer_type == step.materializer_type
                && metadata.resolved_secret == step.resolved_secret
                && metadata.bound_target() == Some(binding)
            {
                if metadata.cleanup_status.is_none() {
                    return Ok(Some(metadata));
                }
                terminal.get_or_insert(metadata);
            }
        }
        if terminal.is_some() {
            return Ok(terminal);
        }
    }

    let key = legacy_materialization_key(
        profile_name,
        consumer,
        &step.materializer_type,
        &step.resolved_secret,
        &step.target,
    );
    let Some((value_json, _version)) = store
        .get_state_kv(CREDENTIAL_MATERIALIZATION_NAMESPACE, &key)
        .map_err(|e| format!("read credential materialization metadata: {e}"))?
    else {
        return Ok(None);
    };
    let metadata = serde_json::from_str(&value_json)
        .map_err(|e| format!("parse credential materialization metadata: {e}"))?;
    Ok(Some(metadata))
}

pub(super) fn close_metadata(
    state: &StateTransaction<'_>,
    row: &memcore::db::StateRow,
    metadata: &mut ManagedCredentialMaterialization,
    status: &str,
    cleanup_run_dir: Option<&Path>,
) -> Result<bool, String> {
    let now = jiff::Timestamp::now();
    metadata.cleanup_status = Some(status.to_owned());
    metadata.cleaned_at = Some(now.to_string());
    metadata.cleanup_run_dir =
        cleanup_run_dir.map(|run_dir| run_dir.to_string_lossy().into_owned());
    metadata.expires_at = Some((now + jiff::SignedDuration::from_secs(30 * 86_400)).to_string());
    let value = serde_json::to_string(metadata)
        .map_err(|err| format!("serialize closed credential metadata: {err}"))?;
    state
        .set_state_if_version(
            CREDENTIAL_MATERIALIZATION_NAMESPACE,
            &row.key,
            &value,
            row.version,
        )
        .map_err(|err| format!("close credential materialization '{}': {err}", row.key))
}

//! Canonical CLI-only GC/delete plan/apply protocol (#1755).
//!
//! This module is intentionally closed: it serves exactly two irreversible
//! operations and is not a generic maintenance framework.

use crate::daemon_lock::{DualDaemonLock, DualLockError};
use crate::db_ownership::{daemon_ownership, DbOwnership};
use memcore::{
    GcConfig, MaintenanceClassFact, MemoryStore, OperatorMaintenanceCommittedReceiptBinding,
    OperatorMaintenancePlanBinding, StoreProfile, OPERATOR_DELETE_CLASSES, OPERATOR_GC_CLASSES,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use tachi_bootstrap::cli::{DeleteAction, GcAction};

const PLAN_VERSION: u32 = 1;
const RECEIPT_VERSION: u32 = 1;
const AUTHORITY_VERSION: u32 = 1;
const POLICY_VERSION: &str = "tachi-memory-maintenance-v1";
const KANBAN_MAX_AGE_DAYS: u64 = crate::kanban::DEFAULT_KANBAN_GC_MAX_AGE_DAYS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MaintenanceOperation {
    Gc,
    Delete,
}

impl MaintenanceOperation {
    fn label(self) -> &'static str {
        match self {
            Self::Gc => "gc",
            Self::Delete => "delete",
        }
    }

    fn classes(self) -> &'static [&'static str] {
        match self {
            Self::Gc => OPERATOR_GC_CLASSES,
            Self::Delete => OPERATOR_DELETE_CLASSES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum MaintenancePolicy {
    Gc {
        access_history_keep_per_memory: usize,
        processed_events_max_days: u32,
        audit_log_max_days: u32,
        audit_log_max_rows: usize,
        agent_known_state_max_days: u32,
        recall_impression_max_groups: usize,
        recall_impression_max_days: u32,
        kanban_max_age_days: u64,
    },
    Delete {
        exact_id_canonical_delete: bool,
    },
}

impl MaintenancePolicy {
    fn canonical_gc() -> Self {
        let cfg = GcConfig::default();
        Self::Gc {
            access_history_keep_per_memory: cfg.access_history_keep_per_memory,
            processed_events_max_days: cfg.processed_events_max_days,
            audit_log_max_days: cfg.audit_log_max_days,
            audit_log_max_rows: cfg.audit_log_max_rows,
            agent_known_state_max_days: cfg.agent_known_state_max_days,
            recall_impression_max_groups: cfg.recall_impression_max_groups,
            recall_impression_max_days: cfg.recall_impression_max_days,
            kanban_max_age_days: KANBAN_MAX_AGE_DAYS,
        }
    }

    fn gc_config(&self) -> Result<(GcConfig, u64), String> {
        let Self::Gc {
            access_history_keep_per_memory,
            processed_events_max_days,
            audit_log_max_days,
            audit_log_max_rows,
            agent_known_state_max_days,
            recall_impression_max_groups,
            recall_impression_max_days,
            kanban_max_age_days,
        } = self
        else {
            return Err("GC plan carries a non-GC policy".to_string());
        };
        Ok((
            GcConfig {
                access_history_keep_per_memory: *access_history_keep_per_memory,
                processed_events_max_days: *processed_events_max_days,
                audit_log_max_days: *audit_log_max_days,
                audit_log_max_rows: *audit_log_max_rows,
                agent_known_state_max_days: *agent_known_state_max_days,
                recall_impression_max_groups: *recall_impression_max_groups,
                recall_impression_max_days: *recall_impression_max_days,
            },
            *kanban_max_age_days,
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaintenancePlan {
    version: u32,
    operation: MaintenanceOperation,
    target_path: String,
    target_physical_identity: String,
    profile: String,
    as_of: String,
    policy_version: String,
    policy: MaintenancePolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    delete_id: Option<String>,
    source: Vec<MaintenanceClassFact>,
    digest: String,
}

impl MaintenancePlan {
    fn digest(&self) -> Result<String, serde_json::Error> {
        let mut unsigned = self.clone();
        unsigned.digest.clear();
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&unsigned)?)
        ))
    }

    fn seal(mut self) -> Result<Self, serde_json::Error> {
        self.digest = self.digest()?;
        Ok(self)
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != PLAN_VERSION || self.policy_version != POLICY_VERSION {
            return Err("unsupported maintenance plan version/policy".to_string());
        }
        if self.target_path.is_empty()
            || self.target_physical_identity.is_empty()
            || !matches!(self.profile.as_str(), "tachi_full" | "portable_kernel")
        {
            return Err("maintenance plan target/profile identity is incomplete".to_string());
        }
        chrono::DateTime::parse_from_rfc3339(&self.as_of)
            .map_err(|error| format!("maintenance plan as_of is invalid: {error}"))?;
        match (self.operation, &self.policy, &self.delete_id) {
            (MaintenanceOperation::Gc, MaintenancePolicy::Gc { .. }, None) => {}
            (
                MaintenanceOperation::Delete,
                MaintenancePolicy::Delete {
                    exact_id_canonical_delete: true,
                },
                Some(id),
            ) if !id.trim().is_empty() && id == id.trim() => {}
            _ => return Err("maintenance plan operation/policy arguments disagree".to_string()),
        }
        if self.operation == MaintenanceOperation::Gc
            && self.policy != MaintenancePolicy::canonical_gc()
        {
            return Err("maintenance plan must retain the canonical GC policy".to_string());
        }
        validate_facts(self.operation, &self.source)?;
        let expected = self
            .digest()
            .map_err(|error| format!("maintenance plan digest serialization failed: {error}"))?;
        if self.digest != expected {
            return Err("maintenance plan digest mismatch".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptPhase {
    Prepared,
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MaintenanceReceipt {
    version: u32,
    plan_digest: String,
    operation: MaintenanceOperation,
    target_path: String,
    target_physical_identity: String,
    profile: String,
    phase: ReceiptPhase,
    apply_timestamp: String,
    source: Vec<MaintenanceClassFact>,
    post: Vec<MaintenanceClassFact>,
    cache_invalidated: bool,
    reconciliation: String,
    digest: String,
}

impl MaintenanceReceipt {
    fn digest(&self) -> Result<String, serde_json::Error> {
        let mut unsigned = self.clone();
        unsigned.digest.clear();
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&unsigned)?)
        ))
    }

    fn seal(mut self) -> Result<Self, serde_json::Error> {
        self.digest = self.digest()?;
        Ok(self)
    }

    fn validate_for_plan(&self, plan: &MaintenancePlan) -> Result<(), String> {
        if self.version != RECEIPT_VERSION
            || self.plan_digest != plan.digest
            || self.operation != plan.operation
            || self.target_path != plan.target_path
            || self.target_physical_identity != plan.target_physical_identity
            || self.profile != plan.profile
            || self.source != plan.source
        {
            return Err("maintenance receipt does not bind the supplied plan".to_string());
        }
        validate_facts(self.operation, &self.source)?;
        validate_facts(self.operation, &self.post)?;
        let expected = self
            .digest()
            .map_err(|error| format!("maintenance receipt digest serialization failed: {error}"))?;
        if self.digest != expected {
            return Err("maintenance receipt digest mismatch".to_string());
        }
        match self.phase {
            ReceiptPhase::Prepared
                if !self.cache_invalidated && self.reconciliation == "required" => {}
            ReceiptPhase::Committed
                if self.cache_invalidated && self.reconciliation == "complete" => {}
            _ => return Err("maintenance receipt phase/accounting is inconsistent".to_string()),
        }
        Ok(())
    }

    fn committed(&self) -> Result<Self, serde_json::Error> {
        let mut committed = self.clone();
        committed.phase = ReceiptPhase::Committed;
        committed.cache_invalidated = true;
        committed.reconciliation = "complete".to_string();
        committed.seal()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommittedReceiptAuthority {
    version: u32,
    plan_digest: String,
    operation: MaintenanceOperation,
    target_physical_identity: String,
    profile: String,
    apply_timestamp: String,
    source: Vec<MaintenanceClassFact>,
    post: Vec<MaintenanceClassFact>,
    committed_receipt_digest: String,
}

impl CommittedReceiptAuthority {
    fn committed_for_plan(&self, plan: &MaintenancePlan) -> Result<MaintenanceReceipt, String> {
        if self.version != AUTHORITY_VERSION
            || self.plan_digest != plan.digest
            || self.operation != plan.operation
            || self.target_physical_identity != plan.target_physical_identity
            || self.profile != plan.profile
            || self.source != plan.source
        {
            return Err("committed maintenance authority does not bind the supplied plan".into());
        }
        chrono::DateTime::parse_from_rfc3339(&self.apply_timestamp).map_err(|error| {
            format!("committed maintenance authority timestamp is invalid: {error}")
        })?;
        validate_facts(self.operation, &self.source)?;
        validate_facts(self.operation, &self.post)?;
        let committed = MaintenanceReceipt {
            version: RECEIPT_VERSION,
            plan_digest: plan.digest.clone(),
            operation: plan.operation,
            target_path: plan.target_path.clone(),
            target_physical_identity: plan.target_physical_identity.clone(),
            profile: plan.profile.clone(),
            phase: ReceiptPhase::Committed,
            apply_timestamp: self.apply_timestamp.clone(),
            source: self.source.clone(),
            post: self.post.clone(),
            cache_invalidated: true,
            reconciliation: "complete".to_string(),
            digest: String::new(),
        }
        .seal()
        .map_err(|error| error.to_string())?;
        if committed.digest != self.committed_receipt_digest {
            return Err("committed maintenance authority receipt digest mismatch".into());
        }
        Ok(committed)
    }
}

fn validate_facts(
    operation: MaintenanceOperation,
    facts: &[MaintenanceClassFact],
) -> Result<(), String> {
    let expected = operation.classes();
    if facts.len() != expected.len()
        || facts
            .iter()
            .map(|fact| fact.class.as_str())
            .ne(expected.iter().copied())
    {
        return Err(format!(
            "{} maintenance class registry is incomplete or reordered",
            operation.label()
        ));
    }
    let mut unique = BTreeSet::new();
    for fact in facts {
        if !unique.insert(&fact.class)
            || fact.digest.len() != 64
            || !fact.digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(format!("invalid maintenance fact for class {}", fact.class));
        }
    }
    Ok(())
}

fn profile_from_plan(plan: &MaintenancePlan) -> Result<StoreProfile, String> {
    StoreProfile::from_stamp_token(&plan.profile)
        .ok_or_else(|| format!("unsupported maintenance profile {}", plan.profile))
}

fn receipt_path(plan_path: &Path) -> PathBuf {
    let mut value = plan_path.as_os_str().to_os_string();
    value.push(".receipt");
    PathBuf::from(value)
}

#[cfg(test)]
fn legacy_recovery_receipt_path(receipt_out: &Path, plan: &MaintenancePlan) -> PathBuf {
    let target_scope = format!(
        "{:x}",
        Sha256::digest(plan.target_physical_identity.as_bytes())
    );
    receipt_out
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            ".tachi-maintenance-recovery-{}-{target_scope}",
            plan.digest
        ))
}

fn artifact_error(
    operation: &str,
    path: &Path,
    error: super::receipt::PreparedArtifactError,
) -> String {
    match error {
        super::receipt::PreparedArtifactError::Stage(error) => {
            format!("{operation} artifact stage/fsync failed at {}: {error}", path.display())
        }
        super::receipt::PreparedArtifactError::AlreadyExists(retained) => {
            format!(
                "{operation} artifact already exists: {}; staged artifact retained at {}",
                path.display(), retained.display()
            )
        }
        super::receipt::PreparedArtifactError::Publish(error) => format!(
            "{operation} artifact atomic publish failed at {}: {error}",
            path.display()
        ),
        super::receipt::PreparedArtifactError::ParentSync(error) => format!(
            "{operation} artifact parent fsync failed at {}: {error}; published prepared evidence was retained",
            path.display()
        ),
    }
}

fn publish_prepared_receipt(path: &Path, bytes: &[u8]) -> Result<File, Box<dyn std::error::Error>> {
    super::receipt::persist_prepared_artifact_bytes(
        path,
        super::receipt::ReceiptKind::MemoryMaintenance,
        bytes,
    )
    .map_err(|error| artifact_error("maintenance prepared receipt", path, error).into())
}

fn publish_artifact(
    operation: &str,
    path: &Path,
    bytes: &[u8],
) -> Result<File, Box<dyn std::error::Error>> {
    super::receipt::persist_prepared_artifact_bytes(
        path,
        super::receipt::ReceiptKind::MemoryMaintenance,
        bytes,
    )
    .map_err(|error| artifact_error(operation, path, error).into())
}

fn acquire_apply_guard(
    operation: MaintenanceOperation,
    target: &Path,
    daemon_scope: &Path,
    app_home: &Path,
) -> Result<DualDaemonLock, Box<dyn std::error::Error>> {
    let lock = match DualDaemonLock::acquire(app_home, daemon_scope) {
        Ok(lock) => lock,
        Err(DualLockError::ScopedRunning { pid }) => {
            return Err(format!(
                "{} apply refused: daemon pid {pid} holds scoped lock",
                operation.label()
            )
            .into())
        }
        Err(DualLockError::LegacyRunning { pid }) => {
            return Err(format!(
                "{} apply refused: daemon pid {pid} holds legacy lock",
                operation.label()
            )
            .into())
        }
        Err(DualLockError::Io(error)) => {
            return Err(format!(
                "{} apply daemon ownership unknown: {error}",
                operation.label()
            )
            .into())
        }
    };
    match daemon_ownership(target) {
        DbOwnership::NotOwned => Ok(lock),
        DbOwnership::Owned => Err(format!(
            "{} apply refused: target DB is owned by a live daemon",
            operation.label()
        )
        .into()),
        DbOwnership::Unknown(reason) => Err(format!(
            "{} apply refused: target DB ownership unknown: {reason}",
            operation.label()
        )
        .into()),
    }
}

fn target_for_plan(
    plan: &MaintenancePlan,
    app_home: &Path,
    require_write: bool,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let (target, daemon_scope) = super::exact_dedupe::manifest_target_and_daemon_scope(
        plan.operation.label(),
        &plan.target_path,
        app_home,
        require_write,
    )?;
    if target.to_string_lossy() != plan.target_path {
        return Err(format!("{} plan target DB mismatch", plan.operation.label()).into());
    }
    Ok((target, daemon_scope))
}

fn verify_store_plan_identity(
    store: &MemoryStore,
    target: &Path,
    plan: &MaintenancePlan,
) -> Result<(), Box<dyn std::error::Error>> {
    store.verify_opened_physical_db_identity(target)?;
    if store.opened_physical_db_identity() != Some(plan.target_physical_identity.as_str()) {
        return Err(format!(
            "{} plan physical DB identity mismatch",
            plan.operation.label()
        )
        .into());
    }
    if store.store_profile() != profile_from_plan(plan)? {
        return Err(format!("{} plan profile mismatch", plan.operation.label()).into());
    }
    Ok(())
}

fn committed_authority_for_plan(
    plan: &MaintenancePlan,
    app_home: &Path,
) -> Result<Option<(CommittedReceiptAuthority, MaintenanceReceipt)>, Box<dyn std::error::Error>> {
    let (target, _) = target_for_plan(plan, app_home, false)?;
    let store = MemoryStore::open_read_only_immutable(&plan.target_path)?;
    verify_store_plan_identity(&store, &target, plan)?;
    let Some(authority_json) = store.operator_maintenance_committed_authority(&plan.digest)? else {
        return Ok(None);
    };
    let authority: CommittedReceiptAuthority = serde_json::from_str(&authority_json)?;
    let committed = authority.committed_for_plan(plan)?;
    if current_facts(&store, plan)? != authority.post {
        return Err(format!(
            "{} committed authority post-state changed under the same plan identity",
            plan.operation.label()
        )
        .into());
    }
    Ok(Some((authority, committed)))
}

fn pretty_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec_pretty(value)
}

fn plan_common(
    operation: MaintenanceOperation,
    db: &Path,
    out: &Path,
    delete_id: Option<String>,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (target, _) = super::exact_dedupe::manifest_target_and_daemon_scope(
        operation.label(),
        &db.to_string_lossy(),
        app_home,
        false,
    )?;
    if out == target || std::fs::canonicalize(out).is_ok_and(|path| path == target) {
        return Err(format!(
            "{} plan output must not be the target DB",
            operation.label()
        )
        .into());
    }
    let target_path = target.to_string_lossy().into_owned();
    let store = MemoryStore::open_read_only_immutable(&target_path)?;
    store.verify_opened_physical_db_identity(&target)?;
    let target_physical_identity = store
        .opened_physical_db_identity()
        .ok_or("maintenance plan target has no stable physical identity")?
        .to_string();
    let as_of = memcore::now_utc_iso();
    let (policy, source) = match operation {
        MaintenanceOperation::Gc => {
            let policy = MaintenancePolicy::canonical_gc();
            let (cfg, kanban_days) = policy.gc_config()?;
            let source = store.plan_operator_gc(&cfg, &as_of, kanban_days, true)?;
            (policy, source)
        }
        MaintenanceOperation::Delete => {
            let id = delete_id
                .as_deref()
                .ok_or("delete plan requires one exact ID")?;
            (
                MaintenancePolicy::Delete {
                    exact_id_canonical_delete: true,
                },
                store.plan_operator_delete(id)?,
            )
        }
    };
    let plan = MaintenancePlan {
        version: PLAN_VERSION,
        operation,
        target_path,
        target_physical_identity,
        profile: store.store_profile().as_str().to_string(),
        as_of,
        policy_version: POLICY_VERSION.to_string(),
        policy,
        delete_id,
        source,
        digest: String::new(),
    }
    .seal()?;
    plan.validate()?;
    let bytes = pretty_bytes(&plan)?;
    drop(publish_artifact(
        &format!("{} plan", operation.label()),
        out,
        &bytes,
    )?);
    println!("{}", String::from_utf8(bytes)?);
    Ok(())
}

pub(crate) fn run_gc(action: GcAction, app_home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        GcAction::Plan { db, out } => {
            plan_common(MaintenanceOperation::Gc, &db, &out, None, app_home)
        }
        GcAction::Apply { plan, yes } => {
            apply_common(&plan, yes, MaintenanceOperation::Gc, app_home)
        }
    }
}

pub(crate) fn run_delete(
    action: DeleteAction,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        DeleteAction::Plan { db, id, out } => {
            plan_common(MaintenanceOperation::Delete, &db, &out, Some(id), app_home)
        }
        DeleteAction::Apply { plan, yes } => {
            apply_common(&plan, yes, MaintenanceOperation::Delete, app_home)
        }
    }
}

fn current_facts(
    store: &MemoryStore,
    plan: &MaintenancePlan,
) -> Result<Vec<MaintenanceClassFact>, Box<dyn std::error::Error>> {
    match plan.operation {
        MaintenanceOperation::Gc => {
            let (cfg, kanban_days) = plan.policy.gc_config()?;
            Ok(store.plan_operator_gc(&cfg, &plan.as_of, kanban_days, true)?)
        }
        MaintenanceOperation::Delete => Ok(store.plan_operator_delete(
            plan.delete_id
                .as_deref()
                .ok_or("validated delete plan lost its exact ID")?,
        )?),
    }
}

fn open_receipt(
    path: &Path,
    plan: &MaintenancePlan,
) -> Result<(MaintenanceReceipt, File, Vec<u8>), Box<dyn std::error::Error>> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let receipt: MaintenanceReceipt = serde_json::from_slice(&bytes)?;
    receipt.validate_for_plan(plan)?;
    Ok((receipt, file, bytes))
}

fn apply_common(
    plan_path: &Path,
    yes: bool,
    expected_operation: MaintenanceOperation,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Confirmation is checked before lock acquisition or any writable open.
    if !yes {
        return Err(format!(
            "{} apply requires --yes; this operation is irreversible and has no restore",
            expected_operation.label()
        )
        .into());
    }
    let plan_bytes = std::fs::read(plan_path)?;
    let plan: MaintenancePlan = serde_json::from_slice(&plan_bytes)?;
    plan.validate()?;
    let operator_plan = OperatorMaintenancePlanBinding::from_canonical_json(&plan_bytes)?;
    if plan.operation != expected_operation {
        return Err("maintenance plan operation does not match CLI command".into());
    }
    let receipt_out = receipt_path(plan_path);
    if receipt_out == PathBuf::from(&plan.target_path) {
        return Err("maintenance receipt output must not be the target DB".into());
    }

    let committed_authority = committed_authority_for_plan(&plan, app_home)?;
    if let Some((_authority, committed)) = committed_authority.as_ref() {
        let bytes = pretty_bytes(committed)?;
        let mut file_bytes = bytes.clone();
        file_bytes.push(b'\n');
        if std::fs::read(&receipt_out).is_ok_and(|public| public == file_bytes) {
            print!("{}", String::from_utf8(bytes)?);
            return Ok(());
        }
    }

    let (target, daemon_scope) = target_for_plan(&plan, app_home, true)?;
    let _lock = acquire_apply_guard(plan.operation, &target, &daemon_scope, app_home)?;
    let mut store = MemoryStore::open_existing_read_write(&plan.target_path)?;
    verify_store_plan_identity(&store, &target, &plan)?;

    if let Some((expected_authority, _)) = committed_authority.as_ref() {
        let current_json = store
            .operator_maintenance_committed_authority(&plan.digest)?
            .ok_or("committed maintenance authority disappeared before replay")?;
        let current_authority: CommittedReceiptAuthority = serde_json::from_str(&current_json)?;
        if &current_authority != expected_authority {
            return Err("committed maintenance authority changed during replay".into());
        }
    }

    let (existing_prepared, existing_prepared_file) = if receipt_out.exists() {
        match open_receipt(&receipt_out, &plan) {
            Ok((receipt, file, _)) if receipt.phase == ReceiptPhase::Prepared => {
                (Some(receipt), Some(file))
            }
            Ok((_receipt, _file, _)) if committed_authority.is_some() => (None, None),
            Ok(_) => {
                return Err(
                    "public committed receipt has no same-transaction database authority".into(),
                )
            }
            Err(_) if committed_authority.is_some() => (None, None),
            Err(error) => return Err(error),
        }
    } else {
        (None, None)
    };
    let current = current_facts(&store, &plan)?;
    if let Some((authority, _)) = committed_authority.as_ref() {
        if current != authority.post {
            return Err(format!(
                "{} committed authority post-state changed under the apply lock",
                plan.operation.label()
            )
            .into());
        }
    } else if let Some(receipt) = existing_prepared.as_ref() {
        if current != receipt.source {
            return Err(format!(
                "{} prepared receipt source facts changed before replay",
                plan.operation.label()
            )
            .into());
        }
    } else if current != plan.source {
        return Err(format!(
            "{} apply source facts drifted from plan",
            plan.operation.label()
        )
        .into());
    }

    let mut prepared_receipt = existing_prepared;
    let mut prepared_file = existing_prepared_file;

    if committed_authority.is_none() {
        let apply_timestamp = prepared_receipt
            .as_ref()
            .map(|receipt| receipt.apply_timestamp.clone())
            .unwrap_or_else(memcore::now_utc_iso);
        let operation = plan.operation;
        #[cfg(test)]
        DB_APPLY_CALLS.with(|count| count.set(count.get() + 1));
        let apply_result = match operation {
            MaintenanceOperation::Gc => store
                .apply_operator_gc_with_precommit_receipt(&operator_plan, |_, source, post| {
                    prepare_or_validate_receipt(
                        &plan,
                        &operator_plan,
                        &receipt_out,
                        &apply_timestamp,
                        source,
                        post,
                        &mut prepared_receipt,
                        &mut prepared_file,
                    )
                })
                .map(|_| ()),
            MaintenanceOperation::Delete => store
                .apply_operator_delete_with_precommit_receipt(&operator_plan, |_, source, post| {
                    prepare_or_validate_receipt(
                        &plan,
                        &operator_plan,
                        &receipt_out,
                        &apply_timestamp,
                        source,
                        post,
                        &mut prepared_receipt,
                        &mut prepared_file,
                    )
                })
                .map(|_| ()),
        };
        if let Err(error) = apply_result {
            let evidence = if receipt_out.exists() {
                format!(
                    "public prepared artifact retained at {} without mutation authority",
                    receipt_out.display()
                )
            } else {
                "no prepared receipt was published".to_string()
            };
            return Err(format!(
                "{} DB transaction failed and rolled back; {evidence}: {error}",
                operation.label()
            )
            .into());
        }
        if let Err(error) = store.verify_opened_physical_db_identity(&target) {
            return Err(format!(
                "{} DB committed but target identity changed; database authority remains durable for reconciliation: {error}",
                operation.label(),
            )
            .into());
        }
        let prepared_file_ref = prepared_file
            .as_ref()
            .ok_or("maintenance apply lost its prepared receipt inode")?;
        match super::receipt::path_names_open_file(&receipt_out, prepared_file_ref) {
            Ok(true) => {}
            Ok(false) => {
                return Err(format!(
                    "{} DB committed after the public prepared receipt changed; database authority retained for reconciliation",
                    plan.operation.label(),
                )
                .into())
            }
            Err(error) => {
                return Err(format!(
                    "{} DB committed but the public prepared receipt could not be revalidated; database authority retained for reconciliation: {error}",
                    plan.operation.label(),
                )
                .into())
            }
        }
    }

    let committed = if let Some((_authority, committed)) = committed_authority {
        committed
    } else {
        prepared_receipt
            .as_ref()
            .ok_or("maintenance apply has no prepared receipt evidence")?
            .committed()?
    };
    #[cfg(test)]
    if let Err(error) = fault_before_cache() {
        return Err(format!(
            "{} DB committed but cache invalidation failed; database authority retained for reconciliation: {error}",
            plan.operation.label(),
        )
        .into());
    }
    if let Err(error) = store.recall_cache_invalidate_all() {
        return Err(format!(
            "{} DB committed but cache invalidation failed; database authority retained for reconciliation: {error}",
            plan.operation.label(),
        )
        .into());
    }
    let committed_bytes = pretty_bytes(&committed)?;
    #[cfg(test)]
    if let Err(error) = fault_before_finalize() {
        return Err(format!(
            "{} DB committed but receipt finalization failed; prepared receipt retained at {} for reconciliation: {error}",
            plan.operation.label(),
            receipt_out.display()
        )
        .into());
    }
    let projection = match super::receipt::publish_committed_projection(
        &receipt_out,
        super::receipt::ReceiptKind::MemoryMaintenance,
        prepared_file.as_ref(),
        &committed_bytes,
    ) {
        Ok(projection) => projection,
        Err(error) => {
            return Err(format!(
                "{} DB committed but receipt finalization failed; database authority and projection artifacts were retained for reconciliation: {error}",
                plan.operation.label(),
            )
            .into())
        }
    };
    if let super::receipt::CommittedProjection::ReplacedForeign { retained } = projection {
        return Err(format!(
            "{} DB committed and the committed projection was restored, but an untrusted public receipt replacement was retained at {}; reconciliation required",
            plan.operation.label(),
            retained.display()
        )
        .into());
    }
    println!("{}", String::from_utf8(committed_bytes)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_or_validate_receipt(
    plan: &MaintenancePlan,
    operator_plan: &OperatorMaintenancePlanBinding,
    receipt_out: &Path,
    apply_timestamp: &str,
    source: &[MaintenanceClassFact],
    post: &[MaintenanceClassFact],
    prepared_receipt: &mut Option<MaintenanceReceipt>,
    prepared_file: &mut Option<File>,
) -> Result<OperatorMaintenanceCommittedReceiptBinding, memcore::MemoryError> {
    if let Some(existing) = prepared_receipt {
        if existing.source != source || existing.post != post {
            return Err(memcore::MemoryError::InvalidArg(
                "prepared maintenance receipt facts changed on replay".to_string(),
            ));
        }
    } else {
        let receipt = MaintenanceReceipt {
            version: RECEIPT_VERSION,
            plan_digest: plan.digest.clone(),
            operation: plan.operation,
            target_path: plan.target_path.clone(),
            target_physical_identity: plan.target_physical_identity.clone(),
            profile: plan.profile.clone(),
            phase: ReceiptPhase::Prepared,
            apply_timestamp: apply_timestamp.to_string(),
            source: source.to_vec(),
            post: post.to_vec(),
            cache_invalidated: false,
            reconciliation: "required".to_string(),
            digest: String::new(),
        }
        .seal()
        .map_err(|error| memcore::MemoryError::InvalidArg(error.to_string()))?;
        let bytes = pretty_bytes(&receipt)
            .map_err(|error| memcore::MemoryError::InvalidArg(error.to_string()))?;
        #[cfg(test)]
        fault_before_prepared_publish()?;
        let file = publish_prepared_receipt(receipt_out, &bytes)
            .map_err(|error| memcore::MemoryError::InvalidArg(error.to_string()))?;
        *prepared_file = Some(file);
        *prepared_receipt = Some(receipt);
    }
    #[cfg(test)]
    fault_after_prepared_publish()?;
    #[cfg(test)]
    fault_replace_prepared_path_before_commit(receipt_out)?;
    let prepared_file = prepared_file.as_ref().ok_or_else(|| {
        memcore::MemoryError::InvalidArg(
            "maintenance apply lost its opened prepared receipt inode before commit".to_string(),
        )
    })?;
    match super::receipt::sync_and_validate_prepared_artifact(receipt_out, prepared_file) {
        Ok(true) => {}
        Ok(false) => {
            return Err(memcore::MemoryError::InvalidArg(format!(
                "prepared receipt pathname no longer names the opened inode before commit: {}",
                receipt_out.display()
            )))
        }
        Err(error) => {
            return Err(memcore::MemoryError::InvalidArg(format!(
                "prepared receipt inode durability could not be revalidated before commit: {error}"
            )))
        }
    }
    #[cfg(test)]
    fault_replace_public_after_precommit_check(receipt_out, plan)?;
    let prepared = prepared_receipt.as_ref().ok_or_else(|| {
        memcore::MemoryError::InvalidArg(
            "maintenance apply lost its prepared receipt before authority write".to_string(),
        )
    })?;
    let committed = prepared
        .committed()
        .map_err(|error| memcore::MemoryError::InvalidArg(error.to_string()))?;
    let committed_bytes = serde_json::to_vec(&committed)
        .map_err(|error| memcore::MemoryError::InvalidArg(error.to_string()))?;
    OperatorMaintenanceCommittedReceiptBinding::from_canonical_json(operator_plan, &committed_bytes)
}

#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
std::thread_local! {
    static FAIL_BEFORE_PREPARED_PUBLISH: Cell<bool> = const { Cell::new(false) };
    static FAIL_AFTER_PREPARED_PUBLISH: Cell<bool> = const { Cell::new(false) };
    static REPLACE_PREPARED_BEFORE_COMMIT: Cell<bool> = const { Cell::new(false) };
    static REPLACE_PUBLIC_AFTER_PRECOMMIT_CHECK: Cell<bool> = const { Cell::new(false) };
    static FAIL_BEFORE_CACHE: Cell<bool> = const { Cell::new(false) };
    static FAIL_BEFORE_FINALIZE: Cell<bool> = const { Cell::new(false) };
    static DB_APPLY_CALLS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn take_fault(
    slot: &'static std::thread::LocalKey<Cell<bool>>,
    message: &str,
) -> Result<(), memcore::MemoryError> {
    if slot.with(|fault| fault.replace(false)) {
        Err(memcore::MemoryError::InvalidArg(message.to_string()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
fn fault_before_prepared_publish() -> Result<(), memcore::MemoryError> {
    take_fault(
        &FAIL_BEFORE_PREPARED_PUBLISH,
        "injected prepared publication failure",
    )
}

#[cfg(test)]
fn fault_after_prepared_publish() -> Result<(), memcore::MemoryError> {
    take_fault(
        &FAIL_AFTER_PREPARED_PUBLISH,
        "injected post-publication precommit failure",
    )
}

#[cfg(test)]
fn replaced_prepared_evidence_path(receipt_out: &Path) -> PathBuf {
    receipt_out.with_extension("prepared-retained")
}

#[cfg(test)]
fn fault_replace_prepared_path_before_commit(
    receipt_out: &Path,
) -> Result<(), memcore::MemoryError> {
    if !REPLACE_PREPARED_BEFORE_COMMIT.with(|fault| fault.replace(false)) {
        return Ok(());
    }
    let retained = replaced_prepared_evidence_path(receipt_out);
    std::fs::rename(receipt_out, &retained)?;
    std::fs::write(receipt_out, b"FOREIGN-REPLACEMENT-EVIDENCE")?;
    Ok(())
}

#[cfg(test)]
fn fault_replace_public_after_precommit_check(
    receipt_out: &Path,
    plan: &MaintenancePlan,
) -> Result<(), memcore::MemoryError> {
    if !REPLACE_PUBLIC_AFTER_PRECOMMIT_CHECK.with(|fault| fault.replace(false)) {
        return Ok(());
    }
    std::fs::remove_file(receipt_out)?;
    let recovery_out = legacy_recovery_receipt_path(receipt_out, plan);
    match std::fs::remove_file(recovery_out) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    std::fs::write(receipt_out, b"FOREIGN-AFTER-CHECK-EVIDENCE")?;
    Ok(())
}

#[cfg(test)]
fn fault_before_cache() -> Result<(), memcore::MemoryError> {
    take_fault(&FAIL_BEFORE_CACHE, "injected postcommit cache failure")
}

#[cfg(test)]
fn fault_before_finalize() -> Result<(), memcore::MemoryError> {
    take_fault(
        &FAIL_BEFORE_FINALIZE,
        "injected postcommit finalization failure",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{DbEntry, DbRole, Manifest};

    struct Fixture {
        _dir: tempfile::TempDir,
        app_home: PathBuf,
        db_path: PathBuf,
        plan_path: PathBuf,
    }

    fn fixture_entry(id: &str, path: &str) -> memcore::MemoryEntry {
        memcore::MemoryEntry {
            id: id.to_string(),
            path: path.to_string(),
            summary: "PRIVATE-SUMMARY-SENTINEL".to_string(),
            text: "PRIVATE-BODY-SENTINEL".to_string(),
            importance: 0.7,
            timestamp: "2026-08-01T00:00:00Z".to_string(),
            valid_from: String::new(),
            valid_until: None,
            category: "fact".to_string(),
            topic: String::new(),
            keywords: Vec::new(),
            persons: Vec::new(),
            entities: Vec::new(),
            location: String::new(),
            source: "test".to_string(),
            scope: "general".to_string(),
            archived: false,
            access_count: 0,
            scored_count: 0,
            last_access: None,
            last_use_at: None,
            revision: 1,
            vector: None,
            retention_policy: None,
            domain: None,
            metadata: serde_json::json!({"secret":"PRIVATE-METADATA-SENTINEL"}),
            recall_count: 0,
            query_diversity: 0,
            tier: "raw".to_string(),
        }
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let app_home = dir.path().join("app-home");
        std::fs::create_dir_all(&app_home).unwrap();
        let db_path = dir.path().join(memcore::MEMORY_DB_FILENAME);
        let mut store = MemoryStore::open(&db_path.to_string_lossy()).unwrap();
        store
            .insert_if_absent(&fixture_entry("delete-me", "/notes/private"))
            .unwrap();
        store
            .recall_cache_store("cache:test", "gen", "query", "[]", 0, false)
            .unwrap();
        drop(store);
        let canonical = std::fs::canonicalize(&db_path).unwrap();
        let mut manifest = Manifest::empty();
        manifest.dbs.push(DbEntry {
            path: canonical.to_string_lossy().into_owned(),
            role: DbRole::Project,
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: String::new(),
            last_classification: "healthy".to_string(),
            scope_hint: "project:test".to_string(),
            notes: String::new(),
        });
        manifest.save(&app_home.join("manifest.json")).unwrap();
        let plan_path = dir.path().join("delete-plan.json");
        Fixture {
            _dir: dir,
            app_home,
            db_path,
            plan_path,
        }
    }

    fn plan_delete(fixture: &Fixture) {
        run_delete(
            DeleteAction::Plan {
                db: fixture.db_path.clone(),
                id: "delete-me".to_string(),
                out: fixture.plan_path.clone(),
            },
            &fixture.app_home,
        )
        .unwrap();
    }

    fn exact_file_bytes(paths: &[PathBuf]) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        paths
            .iter()
            .map(|path| (path.clone(), std::fs::read(path).ok()))
            .collect()
    }

    fn memory_exists(path: &Path) -> bool {
        let store = MemoryStore::open_read_only(&path.to_string_lossy()).unwrap();
        store.get("delete-me").unwrap().is_some()
    }

    #[test]
    fn delete_plan_changes_only_requested_artifact_and_contains_no_private_content() {
        let fixture = fixture();
        let watched = [
            fixture.db_path.clone(),
            PathBuf::from(format!("{}-wal", fixture.db_path.display())),
            PathBuf::from(format!("{}-shm", fixture.db_path.display())),
            fixture.app_home.join("manifest.json"),
            fixture.app_home.join("daemon.lock"),
            fixture.app_home.join("daemon.pid"),
        ];
        let before = exact_file_bytes(&watched);

        plan_delete(&fixture);

        assert_eq!(exact_file_bytes(&watched), before);
        assert!(!receipt_path(&fixture.plan_path).exists());
        let bytes = std::fs::read(&fixture.plan_path).unwrap();
        for sentinel in [
            "PRIVATE-SUMMARY-SENTINEL",
            "PRIVATE-BODY-SENTINEL",
            "PRIVATE-METADATA-SENTINEL",
        ] {
            assert!(
                !bytes
                    .windows(sentinel.len())
                    .any(|window| window == sentinel.as_bytes()),
                "plan leaked {sentinel}"
            );
        }
    }

    #[test]
    fn maintenance_requires_manifest_inventory_membership_before_plan_or_apply() {
        let fixture = fixture();
        let external_db = fixture._dir.path().join("external-memory.db");
        let mut external_store = MemoryStore::open(&external_db.to_string_lossy()).unwrap();
        external_store
            .insert_if_absent(&fixture_entry("delete-me", "/notes/external"))
            .unwrap();
        drop(external_store);

        let external_delete_plan = fixture._dir.path().join("external-delete-plan.json");
        let external_gc_plan = fixture._dir.path().join("external-gc-plan.json");
        let external_before = std::fs::read(&external_db).unwrap();

        for error in [
            run_delete(
                DeleteAction::Plan {
                    db: external_db.clone(),
                    id: "delete-me".to_string(),
                    out: external_delete_plan.clone(),
                },
                &fixture.app_home,
            )
            .expect_err("external delete target must not enter plan authority"),
            run_gc(
                GcAction::Plan {
                    db: external_db.clone(),
                    out: external_gc_plan.clone(),
                },
                &fixture.app_home,
            )
            .expect_err("external GC target must not enter plan authority"),
        ] {
            assert!(
                error
                    .to_string()
                    .contains("did not resolve to exactly one manifest DB"),
                "external target reached synthetic inventory fallback: {error}"
            );
        }
        assert!(!external_delete_plan.exists());
        assert!(!external_gc_plan.exists());
        assert!(!receipt_path(&external_delete_plan).exists());
        assert_eq!(std::fs::read(&external_db).unwrap(), external_before);

        // Freeze a valid plan while the external DB is manifest-backed, then
        // remove that authority. Apply must re-resolve the manifest and refuse
        // before publishing a receipt or opening the DB read-write.
        let manifest_path = fixture.app_home.join("manifest.json");
        let original_manifest = std::fs::read(&manifest_path).unwrap();
        let mut manifest = Manifest::load(&manifest_path).unwrap();
        manifest.dbs.push(DbEntry {
            path: std::fs::canonicalize(&external_db)
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            role: DbRole::Project,
            owner: "tachi".to_string(),
            schema_kind: "tachi".to_string(),
            vec_enabled: true,
            allow_write: true,
            last_doctor_at: String::new(),
            last_classification: "healthy".to_string(),
            scope_hint: "project:external".to_string(),
            notes: String::new(),
        });
        manifest.save(&manifest_path).unwrap();
        run_delete(
            DeleteAction::Plan {
                db: external_db.clone(),
                id: "delete-me".to_string(),
                out: external_delete_plan.clone(),
            },
            &fixture.app_home,
        )
        .unwrap();
        std::fs::write(&manifest_path, original_manifest).unwrap();

        let external_before_apply = std::fs::read(&external_db).unwrap();
        let error = run_delete(
            DeleteAction::Apply {
                plan: external_delete_plan.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect_err("apply must revalidate manifest inventory membership");
        assert!(
            error
                .to_string()
                .contains("did not resolve to exactly one manifest DB"),
            "external apply reached synthetic inventory fallback: {error}"
        );
        assert!(!receipt_path(&external_delete_plan).exists());
        assert_eq!(std::fs::read(&external_db).unwrap(), external_before_apply);
        assert!(memory_exists(&external_db));

        // The exact same plan/apply path remains available for the peer that
        // is still backed by the manifest.
        plan_delete(&fixture);
        run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .unwrap();
        assert!(!memory_exists(&fixture.db_path));
        assert!(receipt_path(&fixture.plan_path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn confirmation_and_ownership_refuse_before_receipt_or_db_mutation() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        for ownership in [
            None,
            Some(DbOwnership::Owned),
            Some(DbOwnership::Unknown("probe-fault".to_string())),
        ] {
            let fixture = fixture();
            plan_delete(&fixture);
            let before = std::fs::read(&fixture.db_path).unwrap();
            let yes = ownership.is_some();
            if let Some(ownership) = ownership {
                set_ownership_inject_for_test(Some(ownership));
            }
            let error = run_delete(
                DeleteAction::Apply {
                    plan: fixture.plan_path.clone(),
                    yes,
                },
                &fixture.app_home,
            )
            .expect_err("precondition must refuse");
            assert!(
                error.to_string().contains("requires --yes")
                    || error.to_string().contains("owned by a live daemon")
                    || error.to_string().contains("ownership unknown"),
                "{error}"
            );
            assert_eq!(std::fs::read(&fixture.db_path).unwrap(), before);
            assert!(!receipt_path(&fixture.plan_path).exists());
            assert!(memory_exists(&fixture.db_path));
        }
    }

    #[cfg(unix)]
    #[test]
    fn postcommit_cache_and_finalize_failures_replay_without_a_second_delete_transaction() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        for fault_kind in ["cache", "finalize"] {
            let fixture = fixture();
            plan_delete(&fixture);
            DB_APPLY_CALLS.with(|count| count.set(0));
            match fault_kind {
                "cache" => FAIL_BEFORE_CACHE.with(|fault| fault.set(true)),
                "finalize" => FAIL_BEFORE_FINALIZE.with(|fault| fault.set(true)),
                _ => unreachable!(),
            }
            set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
            let error = run_delete(
                DeleteAction::Apply {
                    plan: fixture.plan_path.clone(),
                    yes: true,
                },
                &fixture.app_home,
            )
            .expect_err("injected postcommit failure");
            assert!(error.to_string().contains("DB committed"), "{error}");
            assert!(error.to_string().contains("reconciliation"), "{error}");
            assert!(!memory_exists(&fixture.db_path));
            let receipt_out = receipt_path(&fixture.plan_path);
            let prepared: MaintenanceReceipt =
                serde_json::from_slice(&std::fs::read(&receipt_out).unwrap()).unwrap();
            assert_eq!(prepared.phase, ReceiptPhase::Prepared);
            assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);

            set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
            run_delete(
                DeleteAction::Apply {
                    plan: fixture.plan_path.clone(),
                    yes: true,
                },
                &fixture.app_home,
            )
            .expect("replay finalizes committed state");
            assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
            let committed_bytes = std::fs::read(&receipt_out).unwrap();
            let committed: MaintenanceReceipt = serde_json::from_slice(&committed_bytes).unwrap();
            assert_eq!(committed.phase, ReceiptPhase::Committed);

            run_delete(
                DeleteAction::Apply {
                    plan: fixture.plan_path.clone(),
                    yes: true,
                },
                &fixture.app_home,
            )
            .expect("committed replay");
            assert_eq!(std::fs::read(&receipt_out).unwrap(), committed_bytes);
            assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn committed_replay_refuses_a_replaced_target_before_returning_receipt() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        let fixture = fixture();
        plan_delete(&fixture);
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .unwrap();
        let committed = std::fs::read(receipt_path(&fixture.plan_path)).unwrap();

        let displaced = fixture.db_path.with_extension("original");
        std::fs::rename(&fixture.db_path, &displaced).unwrap();
        std::fs::copy(&displaced, &fixture.db_path).unwrap();
        let error = run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect_err("committed replay must not authorize a replacement inode");
        assert!(
            error.to_string().contains("physical DB identity mismatch"),
            "{error}"
        );
        assert_eq!(
            std::fs::read(receipt_path(&fixture.plan_path)).unwrap(),
            committed
        );
    }

    #[cfg(unix)]
    #[test]
    fn committed_replay_refuses_changed_post_state_without_a_second_transaction() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        let fixture = fixture();
        plan_delete(&fixture);
        DB_APPLY_CALLS.with(|count| count.set(0));
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .unwrap();
        let receipt_out = receipt_path(&fixture.plan_path);
        let committed = std::fs::read(&receipt_out).unwrap();
        let mut store =
            MemoryStore::open_existing_read_write(&fixture.db_path.to_string_lossy()).unwrap();
        store
            .insert_if_absent(&fixture_entry("delete-me", "/notes/recreated"))
            .unwrap();
        drop(store);

        let error = run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect_err("committed replay must bind the recorded post-state");
        assert!(error.to_string().contains("post-state changed"), "{error}");
        assert_eq!(std::fs::read(&receipt_out).unwrap(), committed);
        assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
    }

    #[cfg(unix)]
    #[test]
    fn resealed_gc_plan_cannot_invent_a_new_retention_policy() {
        let fixture = fixture();
        let gc_plan = fixture.plan_path.with_file_name("gc-policy-plan.json");
        run_gc(
            GcAction::Plan {
                db: fixture.db_path.clone(),
                out: gc_plan.clone(),
            },
            &fixture.app_home,
        )
        .unwrap();
        let mut plan: MaintenancePlan =
            serde_json::from_slice(&std::fs::read(&gc_plan).unwrap()).unwrap();
        let MaintenancePolicy::Gc {
            access_history_keep_per_memory,
            ..
        } = &mut plan.policy
        else {
            panic!("GC plan must carry GC policy")
        };
        *access_history_keep_per_memory = 0;
        plan = plan.seal().unwrap();
        std::fs::write(&gc_plan, pretty_bytes(&plan).unwrap()).unwrap();

        let error = run_gc(
            GcAction::Apply {
                plan: gc_plan.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect_err("a locally re-sealed plan must not create a new GC policy");
        assert!(error.to_string().contains("canonical GC policy"), "{error}");
        assert!(memory_exists(&fixture.db_path));
        assert!(!receipt_path(&gc_plan).exists());
    }

    #[cfg(unix)]
    #[test]
    fn wrong_digest_and_profile_refuse_without_receipt_or_mutation() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        let digest = fixture();
        plan_delete(&digest);
        let mut plan: MaintenancePlan =
            serde_json::from_slice(&std::fs::read(&digest.plan_path).unwrap()).unwrap();
        plan.digest = "0".repeat(64);
        std::fs::write(&digest.plan_path, pretty_bytes(&plan).unwrap()).unwrap();
        let error = run_delete(
            DeleteAction::Apply {
                plan: digest.plan_path.clone(),
                yes: true,
            },
            &digest.app_home,
        )
        .expect_err("wrong plan digest must refuse");
        assert!(error.to_string().contains("digest mismatch"), "{error}");
        assert!(memory_exists(&digest.db_path));
        assert!(!receipt_path(&digest.plan_path).exists());

        let profile = fixture();
        plan_delete(&profile);
        let mut plan: MaintenancePlan =
            serde_json::from_slice(&std::fs::read(&profile.plan_path).unwrap()).unwrap();
        plan.profile = "portable_kernel".to_string();
        plan = plan.seal().unwrap();
        std::fs::write(&profile.plan_path, pretty_bytes(&plan).unwrap()).unwrap();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        let error = run_delete(
            DeleteAction::Apply {
                plan: profile.plan_path.clone(),
                yes: true,
            },
            &profile.app_home,
        )
        .expect_err("wrong live profile must refuse");
        assert!(
            error.to_string().contains("plan profile mismatch"),
            "{error}"
        );
        assert!(memory_exists(&profile.db_path));
        assert!(!receipt_path(&profile.plan_path).exists());
    }

    #[cfg(unix)]
    #[test]
    fn missing_delete_id_applies_as_an_honest_committed_noop() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        let fixture = fixture();
        let missing_plan = fixture.plan_path.with_file_name("missing-delete-plan.json");
        run_delete(
            DeleteAction::Plan {
                db: fixture.db_path.clone(),
                id: "missing-id".to_string(),
                out: missing_plan.clone(),
            },
            &fixture.app_home,
        )
        .unwrap();
        let plan: MaintenancePlan =
            serde_json::from_slice(&std::fs::read(&missing_plan).unwrap()).unwrap();
        assert!(plan.source.iter().all(|fact| fact.count == 0));

        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        run_delete(
            DeleteAction::Apply {
                plan: missing_plan.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .unwrap();
        assert!(memory_exists(&fixture.db_path));
        let receipt: MaintenanceReceipt =
            serde_json::from_slice(&std::fs::read(receipt_path(&missing_plan)).unwrap()).unwrap();
        assert_eq!(receipt.phase, ReceiptPhase::Committed);
        assert!(receipt.source.iter().all(|fact| fact.count == 0));
        assert!(receipt.post.iter().all(|fact| fact.count == 0));
    }

    #[cfg(unix)]
    #[test]
    fn source_drift_and_precommit_failure_never_commit_unplanned_delete() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};
        let drift = fixture();
        plan_delete(&drift);
        let mut store =
            MemoryStore::open_existing_read_write(&drift.db_path.to_string_lossy()).unwrap();
        let current = store.get("delete-me").unwrap().unwrap();
        store
            .update_with_revision(
                "delete-me",
                &current.text,
                "changed after plan",
                &current.source,
                &current.metadata,
                None,
                current.revision,
            )
            .unwrap();
        drop(store);
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        let error = run_delete(
            DeleteAction::Apply {
                plan: drift.plan_path.clone(),
                yes: true,
            },
            &drift.app_home,
        )
        .expect_err("source drift must refuse");
        assert!(
            error.to_string().contains("source facts drifted"),
            "{error}"
        );
        assert!(memory_exists(&drift.db_path));
        assert!(!receipt_path(&drift.plan_path).exists());

        let precommit = fixture();
        plan_delete(&precommit);
        FAIL_AFTER_PREPARED_PUBLISH.with(|fault| fault.set(true));
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        let error = run_delete(
            DeleteAction::Apply {
                plan: precommit.plan_path.clone(),
                yes: true,
            },
            &precommit.app_home,
        )
        .expect_err("injected precommit failure must roll back");
        assert!(error.to_string().contains("rolled back"), "{error}");
        assert!(memory_exists(&precommit.db_path));
        let public = receipt_path(&precommit.plan_path);
        let receipt: MaintenanceReceipt =
            serde_json::from_slice(&std::fs::read(&public).unwrap()).unwrap();
        assert_eq!(receipt.phase, ReceiptPhase::Prepared);

        let publish = fixture();
        plan_delete(&publish);
        FAIL_BEFORE_PREPARED_PUBLISH.with(|fault| fault.set(true));
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        let error = run_delete(
            DeleteAction::Apply {
                plan: publish.plan_path.clone(),
                yes: true,
            },
            &publish.app_home,
        )
        .expect_err("prepared publication failure must roll back");
        assert!(error.to_string().contains("rolled back"), "{error}");
        assert!(memory_exists(&publish.db_path));
        assert!(!receipt_path(&publish.plan_path).exists());

        let recovery_collision = fixture();
        plan_delete(&recovery_collision);
        let collision_plan: MaintenancePlan =
            serde_json::from_slice(&std::fs::read(&recovery_collision.plan_path).unwrap()).unwrap();
        let collision_public = receipt_path(&recovery_collision.plan_path);
        let collision_recovery = legacy_recovery_receipt_path(&collision_public, &collision_plan);
        std::fs::write(&collision_recovery, b"FOREIGN-LEGACY-RECOVERY").unwrap();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        run_delete(
            DeleteAction::Apply {
                plan: recovery_collision.plan_path.clone(),
                yes: true,
            },
            &recovery_collision.app_home,
        )
        .expect("legacy recovery collision is outside the closed authority namespace");
        assert!(!memory_exists(&recovery_collision.db_path));
        assert_eq!(
            std::fs::read(&collision_recovery).unwrap(),
            b"FOREIGN-LEGACY-RECOVERY"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prepared_path_replacement_before_commit_rolls_back_fresh_and_replay() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        for replay in [false, true] {
            let fixture = fixture();
            plan_delete(&fixture);
            let receipt_out = receipt_path(&fixture.plan_path);

            if replay {
                FAIL_AFTER_PREPARED_PUBLISH.with(|fault| fault.set(true));
                set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
                let error = run_delete(
                    DeleteAction::Apply {
                        plan: fixture.plan_path.clone(),
                        yes: true,
                    },
                    &fixture.app_home,
                )
                .expect_err("fixture must retain a prepared replay receipt");
                assert!(error.to_string().contains("rolled back"), "{error}");
                assert!(receipt_out.exists());
                assert!(memory_exists(&fixture.db_path));
            }

            REPLACE_PREPARED_BEFORE_COMMIT.with(|fault| fault.set(true));
            set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
            let error = run_delete(
                DeleteAction::Apply {
                    plan: fixture.plan_path.clone(),
                    yes: true,
                },
                &fixture.app_home,
            )
            .expect_err("prepared pathname replacement must refuse before commit");

            assert!(error.to_string().contains("rolled back"), "{error}");
            assert!(
                error.to_string().contains("prepared receipt pathname"),
                "{error}"
            );
            assert!(memory_exists(&fixture.db_path));
            assert_eq!(
                std::fs::read(&receipt_out).unwrap(),
                b"FOREIGN-REPLACEMENT-EVIDENCE"
            );
            let retained: MaintenanceReceipt = serde_json::from_slice(
                &std::fs::read(replaced_prepared_evidence_path(&receipt_out)).unwrap(),
            )
            .unwrap();
            assert_eq!(retained.phase, ReceiptPhase::Prepared);
        }
    }

    #[cfg(unix)]
    #[test]
    fn postcheck_public_replacement_commits_once_then_replays_from_db_authority() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        let fixture = fixture();
        plan_delete(&fixture);
        let plan: MaintenancePlan =
            serde_json::from_slice(&std::fs::read(&fixture.plan_path).unwrap()).unwrap();
        let receipt_out = receipt_path(&fixture.plan_path);
        let target_scope = format!(
            "{:x}",
            Sha256::digest(plan.target_physical_identity.as_bytes())
        );
        let recovery = receipt_out.parent().unwrap().join(format!(
            ".tachi-maintenance-recovery-{}-{target_scope}",
            plan.digest
        ));

        DB_APPLY_CALLS.with(|count| count.set(0));
        REPLACE_PUBLIC_AFTER_PRECOMMIT_CHECK.with(|fault| fault.set(true));
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        let error = run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect_err("postcheck replacement must return reconciliation, not success");

        assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
        assert!(!memory_exists(&fixture.db_path));
        assert_eq!(
            std::fs::read(&receipt_out).unwrap(),
            b"FOREIGN-AFTER-CHECK-EVIDENCE"
        );
        assert!(!recovery.exists());
        let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
        let authority_json: String = conn
            .query_row(
                "SELECT value_json FROM hard_state
                 WHERE namespace='operator_maintenance_receipt' AND key=?1",
                [&plan.digest],
                |row| row.get(0),
            )
            .expect("same-transaction committed authority must survive both path losses");
        assert!(error.to_string().contains("DB committed"), "{error}");
        assert!(error.to_string().contains("reconciliation"), "{error}");
        let authority: serde_json::Value = serde_json::from_str(&authority_json).unwrap();
        assert_eq!(authority["plan_digest"], plan.digest);
        assert!(
            !authority_json.contains("PRIVATE-BODY-SENTINEL")
                && !authority_json.contains("PRIVATE-SUMMARY-SENTINEL")
                && !authority_json.contains("PRIVATE-METADATA-SENTINEL")
        );
        let expected_committed_digest = authority["committed_receipt_digest"]
            .as_str()
            .expect("typed committed receipt digest")
            .to_string();
        drop(conn);

        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        let replay_error = run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect_err("replay must retain and report the untrusted foreign projection");
        assert!(
            replay_error.to_string().contains("reconciliation required"),
            "{replay_error}"
        );
        assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
        let committed_bytes = std::fs::read(&receipt_out).unwrap();
        let committed: MaintenanceReceipt = serde_json::from_slice(&committed_bytes).unwrap();
        assert_eq!(committed.phase, ReceiptPhase::Committed);
        assert_eq!(committed.digest, expected_committed_digest);
        assert!(fixture._dir.path().read_dir().unwrap().any(|entry| {
            std::fs::read(entry.unwrap().path())
                .is_ok_and(|bytes| bytes == b"FOREIGN-AFTER-CHECK-EVIDENCE")
        }));

        run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect("committed replay must be byte-identical");
        assert_eq!(std::fs::read(&receipt_out).unwrap(), committed_bytes);
        assert_eq!(DB_APPLY_CALLS.with(Cell::get), 1);
    }

    #[cfg(unix)]
    #[test]
    fn recovery_cleanup_never_deletes_a_foreign_replacement() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        let fixture = fixture();
        plan_delete(&fixture);
        let plan: MaintenancePlan =
            serde_json::from_slice(&std::fs::read(&fixture.plan_path).unwrap()).unwrap();
        let receipt_out = receipt_path(&fixture.plan_path);
        let recovery = legacy_recovery_receipt_path(&receipt_out, &plan);
        std::fs::write(&recovery, b"FOREIGN-RECOVERY-CLEANUP-EVIDENCE").unwrap();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect("foreign legacy recovery path must not participate in apply");
        assert!(!memory_exists(&fixture.db_path));
        assert_eq!(
            std::fs::read(&recovery).unwrap(),
            b"FOREIGN-RECOVERY-CLEANUP-EVIDENCE"
        );
        run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect("committed replay must ignore and retain the foreign legacy recovery path");
        assert_eq!(
            std::fs::read(&recovery).unwrap(),
            b"FOREIGN-RECOVERY-CLEANUP-EVIDENCE"
        );
        assert!(!memory_exists(&fixture.db_path));
    }

    #[test]
    fn committed_authority_namespace_rejects_general_state_mutators() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        let fixture = fixture();
        plan_delete(&fixture);
        let plan: MaintenancePlan =
            serde_json::from_slice(&std::fs::read(&fixture.plan_path).unwrap()).unwrap();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .unwrap();

        let store =
            MemoryStore::open_existing_read_write(&fixture.db_path.to_string_lossy()).unwrap();
        let namespace = "operator_maintenance_receipt";
        assert!(store.set_state(namespace, &plan.digest, "{}").is_err());
        assert!(store
            .set_state_if_version(namespace, &plan.digest, "{}", 1)
            .is_err());
        assert!(store.delete_state(namespace, &plan.digest).is_err());
        assert!(store
            .insert_state_if_absent(namespace, "foreign-key", "{}")
            .is_err());
        assert!(store
            .operator_maintenance_committed_authority(&plan.digest)
            .unwrap()
            .is_some());
    }

    #[test]
    fn public_operator_apply_cannot_forge_arbitrary_authority_json() {
        let fixture = fixture();
        plan_delete(&fixture);
        let plan_bytes = std::fs::read(&fixture.plan_path).unwrap();
        let plan: MaintenancePlan = serde_json::from_slice(&plan_bytes).unwrap();
        let operator_plan =
            OperatorMaintenancePlanBinding::from_canonical_json(&plan_bytes).unwrap();
        let mut store =
            MemoryStore::open_existing_read_write(&fixture.db_path.to_string_lossy()).unwrap();
        let error = store
            .apply_operator_delete_with_precommit_receipt(&operator_plan, |_tx, source, post| {
                let receipt = MaintenanceReceipt {
                    version: RECEIPT_VERSION,
                    plan_digest: plan.digest.clone(),
                    operation: plan.operation,
                    target_path: plan.target_path.clone(),
                    target_physical_identity: plan.target_physical_identity.clone(),
                    profile: plan.profile.clone(),
                    phase: ReceiptPhase::Committed,
                    apply_timestamp: "2026-08-13T00:00:00Z".to_string(),
                    source: source.to_vec(),
                    post: post.to_vec(),
                    cache_invalidated: true,
                    reconciliation: "complete".to_string(),
                    digest: String::new(),
                }
                .seal()
                .unwrap();
                let mut value = serde_json::to_value(receipt).unwrap();
                value["body"] = serde_json::json!("forged maintenance body");
                OperatorMaintenanceCommittedReceiptBinding::from_canonical_json(
                    &operator_plan,
                    &serde_json::to_vec(&value).unwrap(),
                )
            })
            .expect_err("unknown/body receipt authority must roll back");
        assert!(error.to_string().contains("unknown field"), "{error}");
        assert!(memory_exists(&fixture.db_path));
        assert!(store
            .get_state_kv("operator_maintenance_receipt", &plan.digest)
            .unwrap()
            .is_none());
    }

    #[test]
    fn same_operation_cannot_forge_independent_authority_bindings() {
        let mut violations = Vec::new();
        for forged_field in [
            "plan_digest",
            "physical_identity",
            "profile",
            "receipt_digest",
        ] {
            let fixture = fixture();
            plan_delete(&fixture);
            let mut plan: MaintenancePlan =
                serde_json::from_slice(&std::fs::read(&fixture.plan_path).unwrap()).unwrap();
            match forged_field {
                "plan_digest" => plan.digest = "a".repeat(64),
                "physical_identity" => {
                    plan.target_physical_identity = "unix:9:9".to_string();
                    plan = plan.seal().unwrap();
                }
                "profile" => {
                    plan.profile = "portable_kernel".to_string();
                    plan = plan.seal().unwrap();
                }
                "receipt_digest" => {}
                _ => unreachable!(),
            }
            let plan_bytes = serde_json::to_vec(&plan).unwrap();
            let mut store =
                MemoryStore::open_existing_read_write(&fixture.db_path.to_string_lossy()).unwrap();
            let result = OperatorMaintenancePlanBinding::from_canonical_json(&plan_bytes).and_then(
                |operator_plan| {
                    store.apply_operator_delete_with_precommit_receipt(
                        &operator_plan,
                        |_tx, source, post| {
                            let mut receipt = MaintenanceReceipt {
                                version: RECEIPT_VERSION,
                                plan_digest: plan.digest.clone(),
                                operation: plan.operation,
                                target_path: plan.target_path.clone(),
                                target_physical_identity: plan.target_physical_identity.clone(),
                                profile: plan.profile.clone(),
                                phase: ReceiptPhase::Committed,
                                apply_timestamp: "2026-08-13T00:00:00Z".to_string(),
                                source: source.to_vec(),
                                post: post.to_vec(),
                                cache_invalidated: true,
                                reconciliation: "complete".to_string(),
                                digest: String::new(),
                            }
                            .seal()
                            .unwrap();
                            if forged_field == "receipt_digest" {
                                receipt.digest = "c".repeat(64);
                            }
                            OperatorMaintenanceCommittedReceiptBinding::from_canonical_json(
                                &operator_plan,
                                &serde_json::to_vec(&receipt).unwrap(),
                            )
                        },
                    )
                },
            );
            let row_exists = store
                .get_state_kv("operator_maintenance_receipt", &plan.digest)
                .unwrap()
                .is_some();
            if result.is_ok() || !memory_exists(&fixture.db_path) || row_exists {
                violations.push(forged_field);
            }
        }
        assert!(
            violations.is_empty(),
            "same-operation forgeries committed instead of rolling back: {violations:?}"
        );
    }

    #[test]
    fn ttl_backfill_and_reaper_never_mutate_committed_authority() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        let fixture = fixture();
        plan_delete(&fixture);
        let plan: MaintenancePlan =
            serde_json::from_slice(&std::fs::read(&fixture.plan_path).unwrap()).unwrap();
        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .unwrap();
        let committed_bytes = std::fs::read(receipt_path(&fixture.plan_path)).unwrap();
        let store =
            MemoryStore::open_existing_read_write(&fixture.db_path.to_string_lossy()).unwrap();
        let before = store
            .get_state_kv("operator_maintenance_receipt", &plan.digest)
            .unwrap()
            .expect("same-transaction authority");
        assert_eq!(
            store
                .backfill_missing_expires_at(
                    "operator_maintenance_receipt",
                    "2020-01-01T00:00:00Z",
                    None,
                )
                .unwrap(),
            0
        );
        assert_eq!(store.reap_expired_state("2026-08-13T00:00:00Z").unwrap(), 0);
        assert_eq!(
            store
                .get_state_kv("operator_maintenance_receipt", &plan.digest)
                .unwrap()
                .unwrap(),
            before,
            "authority bytes and version must remain unchanged"
        );
        drop(store);
        run_delete(
            DeleteAction::Apply {
                plan: fixture.plan_path.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .expect("authority-backed replay remains valid after TTL maintenance");
        assert_eq!(
            std::fs::read(receipt_path(&fixture.plan_path)).unwrap(),
            committed_bytes
        );
    }

    #[cfg(unix)]
    #[test]
    fn gc_plan_and_apply_execute_the_complete_frozen_registry() {
        use crate::db_ownership::{set_ownership_inject_for_test, DbOwnership};

        let fixture = fixture();
        let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
        conn.execute(
            "INSERT INTO processed_events(event_hash,event_id,worker,created_at)
             VALUES ('old-gc','event','worker','2020-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        drop(conn);
        let gc_plan = fixture.plan_path.with_file_name("gc-plan.json");
        run_gc(
            GcAction::Plan {
                db: fixture.db_path.clone(),
                out: gc_plan.clone(),
            },
            &fixture.app_home,
        )
        .unwrap();
        let plan: MaintenancePlan =
            serde_json::from_slice(&std::fs::read(&gc_plan).unwrap()).unwrap();
        plan.validate().unwrap();
        assert_eq!(plan.operation, MaintenanceOperation::Gc);
        assert_eq!(plan.source.len(), OPERATOR_GC_CLASSES.len());
        assert_eq!(
            plan.source
                .iter()
                .find(|fact| fact.class == "processed_events_age")
                .map(|fact| fact.count),
            Some(1)
        );

        set_ownership_inject_for_test(Some(DbOwnership::NotOwned));
        run_gc(
            GcAction::Apply {
                plan: gc_plan.clone(),
                yes: true,
            },
            &fixture.app_home,
        )
        .unwrap();
        let conn = rusqlite::Connection::open(&fixture.db_path).unwrap();
        assert_eq!(
            conn.query_row::<i64, _, _>(
                "SELECT COUNT(*) FROM processed_events WHERE event_hash='old-gc'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            0
        );
        let receipt: MaintenanceReceipt =
            serde_json::from_slice(&std::fs::read(receipt_path(&gc_plan)).unwrap()).unwrap();
        assert_eq!(receipt.phase, ReceiptPhase::Committed);
    }

    #[test]
    fn plan_and_receipt_serialization_is_recursively_body_free() {
        let empty = |class: &&str| MaintenanceClassFact {
            class: (*class).to_string(),
            count: 0,
            digest: format!("{:x}", Sha256::digest([])),
        };
        let plan = MaintenancePlan {
            version: PLAN_VERSION,
            operation: MaintenanceOperation::Delete,
            target_path: "/tmp/tachi-memory.db".to_string(),
            target_physical_identity: "unix:1:2".to_string(),
            profile: "tachi_full".to_string(),
            as_of: "2026-08-13T00:00:00Z".to_string(),
            policy_version: POLICY_VERSION.to_string(),
            policy: MaintenancePolicy::Delete {
                exact_id_canonical_delete: true,
            },
            delete_id: Some("safe-id".to_string()),
            source: OPERATOR_DELETE_CLASSES.iter().map(empty).collect(),
            digest: String::new(),
        }
        .seal()
        .unwrap();
        let receipt = MaintenanceReceipt {
            version: RECEIPT_VERSION,
            plan_digest: plan.digest.clone(),
            operation: plan.operation,
            target_path: plan.target_path.clone(),
            target_physical_identity: plan.target_physical_identity.clone(),
            profile: plan.profile.clone(),
            phase: ReceiptPhase::Prepared,
            apply_timestamp: plan.as_of.clone(),
            source: plan.source.clone(),
            post: plan.source.clone(),
            cache_invalidated: false,
            reconciliation: "required".to_string(),
            digest: String::new(),
        }
        .seal()
        .unwrap();
        for value in [
            serde_json::to_value(plan).unwrap(),
            serde_json::to_value(receipt).unwrap(),
        ] {
            recursively_assert_body_free(&value);
        }
    }

    fn recursively_assert_body_free(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(object) => {
                for (key, value) in object {
                    assert!(
                        !matches!(
                            key.as_str(),
                            "text" | "summary" | "body" | "vector" | "embedding" | "metadata"
                        ),
                        "private content key leaked: {key}"
                    );
                    recursively_assert_body_free(value);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    recursively_assert_body_free(value);
                }
            }
            _ => {}
        }
    }
}

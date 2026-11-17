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
const GC_POLICY_VERSION: &str = "tachi-memory-maintenance-v2";
const KANBAN_MAX_AGE_DAYS: u64 = crate::kanban::DEFAULT_KANBAN_GC_MAX_AGE_DAYS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MaintenanceOperation {
    Gc,
    Delete,
}

impl MaintenanceOperation {
    fn policy_version(self) -> &'static str {
        match self {
            Self::Gc => GC_POLICY_VERSION,
            Self::Delete => POLICY_VERSION,
        }
    }

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
        if self.version != PLAN_VERSION || self.policy_version != self.operation.policy_version() {
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
        policy_version: operation.policy_version().to_string(),
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

#[derive(Serialize)]
struct CandidateInspection {
    version: u32,
    operation: &'static str,
    apply_authority: bool,
    target_path: String,
    target_physical_identity: String,
    profile: String,
    policy_version: &'static str,
    #[serde(flatten)]
    discovery: memcore::db::MaintenanceCandidateReport,
}

fn candidate_report(
    db: &Path,
    prefix: &str,
    out: &Path,
    app_home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let (target, _) = super::exact_dedupe::manifest_target_and_daemon_scope(
        "gc candidates",
        &db.to_string_lossy(),
        app_home,
        false,
    )?;
    if out == target || std::fs::canonicalize(out).is_ok_and(|path| path == target) {
        return Err("candidate report output must not be the target DB".into());
    }
    let store = MemoryStore::open_read_only_immutable(&target.to_string_lossy())?;
    store.verify_opened_physical_db_identity(&target)?;
    let report = CandidateInspection {
        version: 1,
        operation: "gc_candidates",
        apply_authority: false,
        target_path: target.to_string_lossy().into_owned(),
        target_physical_identity: store
            .opened_physical_db_identity()
            .ok_or("candidate report target has no stable physical identity")?
            .to_string(),
        profile: store.store_profile().as_str().to_string(),
        policy_version: GC_POLICY_VERSION,
        discovery: store.inspect_maintenance_candidates(
            prefix,
            &memcore::now_utc_iso(),
            KANBAN_MAX_AGE_DAYS,
        )?,
    };
    let bytes = pretty_bytes(&report)?;
    drop(publish_artifact("gc candidates", out, &bytes)?);
    println!("{}", String::from_utf8(bytes)?);
    Ok(())
}

pub(crate) fn run_gc(action: GcAction, app_home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        GcAction::Candidates { db, prefix, out } => candidate_report(&db, &prefix, &out, app_home),
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
                .apply_operator_gc_with_precommit_receipt(&operator_plan, |source, post| {
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
                .apply_operator_delete_with_precommit_receipt(&operator_plan, |source, post| {
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
mod tests;

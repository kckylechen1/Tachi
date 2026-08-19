use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[test]
fn model_derived_producers_use_receipt_bearing_llm_apis() {
    let actual = discover_production_model_calls();
    let registry = model_call_registry();

    let failures = validate_model_call_census(&actual, &registry);
    assert!(
        failures.is_empty(),
        "model-call census must match typed registry slots:\n{}",
        failures.join("\n\n")
    );
}

fn validate_model_call_census(
    actual: &[ActualModelCall],
    registry: &[ModelCallRecord],
) -> Vec<String> {
    let mut failures = Vec::new();

    let mut registry_counts = BTreeMap::<String, usize>::new();
    for record in registry {
        *registry_counts.entry(record.key()).or_default() += 1;
    }
    let duplicate_registry = registry_counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(key, count)| format!("{key} ({count} entries)"))
        .collect::<Vec<_>>();
    if !duplicate_registry.is_empty() {
        failures.push(format!(
            "duplicate model-call registry entries:\n{}",
            duplicate_registry.join("\n")
        ));
    }

    let mut actual_counts = BTreeMap::<String, Vec<String>>::new();
    for call in actual {
        actual_counts
            .entry(call.key())
            .or_default()
            .push(call.diagnostic());
    }
    let duplicate_actual = actual_counts
        .iter()
        .filter(|(_, calls)| calls.len() > 1)
        .map(|(key, calls)| format!("{key}\n  {}", calls.join("\n  ")))
        .collect::<Vec<_>>();
    if !duplicate_actual.is_empty() {
        failures.push(format!(
            "duplicate discovered production model callsites:\n{}",
            duplicate_actual.join("\n")
        ));
    }

    let mut actual_matches = vec![Vec::<usize>::new(); actual.len()];
    let mut registry_matches = vec![Vec::<usize>::new(); registry.len()];
    for (actual_idx, call) in actual.iter().enumerate() {
        for (record_idx, record) in registry.iter().enumerate() {
            if record.matches(call) {
                actual_matches[actual_idx].push(record_idx);
                registry_matches[record_idx].push(actual_idx);
            }
        }
    }

    let unregistered_actual = actual
        .iter()
        .zip(&actual_matches)
        .filter(|(_, matches)| matches.is_empty())
        .map(|(call, _)| call.diagnostic())
        .collect::<Vec<_>>();
    if !unregistered_actual.is_empty() {
        failures.push(format!(
            "production model callsite(s) missing typed registry slot entries:\n{}",
            unregistered_actual.join("\n")
        ));
    }

    let ambiguous_actual = actual
        .iter()
        .zip(&actual_matches)
        .filter(|(_, matches)| matches.len() > 1)
        .map(|(call, matches)| {
            format!(
                "{}\n  matched slots: {}",
                call.diagnostic(),
                matches
                    .iter()
                    .map(|idx| registry[*idx].slot)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
        .collect::<Vec<_>>();
    if !ambiguous_actual.is_empty() {
        failures.push(format!(
            "production model callsite(s) matched multiple registry slots:\n{}",
            ambiguous_actual.join("\n")
        ));
    }

    let stale_registry = registry
        .iter()
        .zip(&registry_matches)
        .filter(|(_, matches)| matches.is_empty())
        .map(|(record, _)| record.diagnostic())
        .collect::<Vec<_>>();
    if !stale_registry.is_empty() {
        failures.push(format!(
            "typed model-call registry slots no longer match production callsites:\n{}",
            stale_registry.join("\n")
        ));
    }

    let overmatched_registry = registry
        .iter()
        .zip(&registry_matches)
        .filter(|(_, matches)| matches.len() > 1)
        .map(|(record, matches)| {
            format!(
                "{}\n  matched actuals:\n  {}",
                record.diagnostic(),
                matches
                    .iter()
                    .map(|idx| actual[*idx].diagnostic())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            )
        })
        .collect::<Vec<_>>();
    if !overmatched_registry.is_empty() {
        failures.push(format!(
            "typed model-call registry slot(s) matched multiple production callsites; current and successor APIs must not both be present:\n{}",
            overmatched_registry.join("\n")
        ));
    }

    let unknown = registry
        .iter()
        .filter(|record| record.classification == ModelCallClassification::Unknown)
        .map(ModelCallRecord::diagnostic)
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        failures.push(format!(
            "model-call registry entries must not use Unknown classification:\n{}",
            unknown.join("\n")
        ));
    }

    let covered_text_only = registry
        .iter()
        .filter(|record| {
            record.classification == ModelCallClassification::Covered1522
                && record
                    .accepted_apis()
                    .iter()
                    .any(|api| api.drops_receipt_for_covered_1522())
        })
        .map(ModelCallRecord::diagnostic)
        .collect::<Vec<_>>();
    if !covered_text_only.is_empty() {
        failures.push(format!(
            "Covered1522 durable producer slots must use receipt-bearing APIs, not text-only APIs or record_call:\n{}",
            covered_text_only.join("\n")
        ));
    }

    let covered_receipt_dropping_actual = actual
        .iter()
        .zip(&actual_matches)
        .filter(|(call, matches)| {
            matches.len() == 1
                && registry[matches[0]].classification == ModelCallClassification::Covered1522
                && call.api.drops_receipt_for_covered_1522()
        })
        .map(|(call, _)| call.diagnostic())
        .collect::<Vec<_>>();
    if !covered_receipt_dropping_actual.is_empty() {
        failures.push(format!(
            "Covered1522 durable producer actual callsites must not drop receipts, including record_call:\n{}",
            covered_receipt_dropping_actual.join("\n")
        ));
    }

    let missing_disposition = registry
        .iter()
        .filter(|record| {
            record
                .accepted_apis()
                .iter()
                .any(|api| api.is_receipt_bearing())
                && record.disposition.is_unknown()
        })
        .map(ModelCallRecord::diagnostic)
        .collect::<Vec<_>>();
    if !missing_disposition.is_empty() {
        failures.push(format!(
            "receipt-bearing model callsites must declare their first sink/disposition:\n{}",
            missing_disposition.join("\n")
        ));
    }

    failures
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ModelApi {
    RecordCall,
    RecordCallWithReceipt,
    CallExtractLlm,
    CallExtractLlmWithReceipt,
    CallDistillLlm,
    CallDistillLlmWithReceipt,
    CallSummaryLlm,
    CallSummaryLlmWithReceipt,
    CallReasoningLlm,
    CallReasoningLlmWithReceipt,
    CallReasoningLlmProviderOnly,
    CallReasoningLlmProviderOnlyWithReceipt,
    GenerateSummary,
    GenerateSummaryWithReceipt,
    GenerateDistill,
    GenerateDistillWithReceipt,
    ExtractMetadata,
    ExtractMetadataWithReceipt,
    ExpandSearchKeywords,
    ExpandSearchKeywordsWithReceipt,
    ExtractFacts,
    ExtractFactsWithReceipt,
}

impl ModelApi {
    fn as_str(self) -> &'static str {
        match self {
            Self::RecordCall => "record_call",
            Self::RecordCallWithReceipt => "record_call_with_receipt",
            Self::CallExtractLlm => "call_extract_llm",
            Self::CallExtractLlmWithReceipt => "call_extract_llm_with_receipt",
            Self::CallDistillLlm => "call_distill_llm",
            Self::CallDistillLlmWithReceipt => "call_distill_llm_with_receipt",
            Self::CallSummaryLlm => "call_summary_llm",
            Self::CallSummaryLlmWithReceipt => "call_summary_llm_with_receipt",
            Self::CallReasoningLlm => "call_reasoning_llm",
            Self::CallReasoningLlmWithReceipt => "call_reasoning_llm_with_receipt",
            Self::CallReasoningLlmProviderOnly => "call_reasoning_llm_provider_only",
            Self::CallReasoningLlmProviderOnlyWithReceipt => {
                "call_reasoning_llm_provider_only_with_receipt"
            }
            Self::GenerateSummary => "generate_summary",
            Self::GenerateSummaryWithReceipt => "generate_summary_with_receipt",
            Self::GenerateDistill => "generate_distill",
            Self::GenerateDistillWithReceipt => "generate_distill_with_receipt",
            Self::ExtractMetadata => "extract_metadata",
            Self::ExtractMetadataWithReceipt => "extract_metadata_with_receipt",
            Self::ExpandSearchKeywords => "expand_search_keywords",
            Self::ExpandSearchKeywordsWithReceipt => "expand_search_keywords_with_receipt",
            Self::ExtractFacts => "extract_facts",
            Self::ExtractFactsWithReceipt => "extract_facts_with_receipt",
        }
    }

    fn is_text_only_model_api(self) -> bool {
        matches!(
            self,
            Self::CallExtractLlm
                | Self::CallDistillLlm
                | Self::CallSummaryLlm
                | Self::CallReasoningLlm
                | Self::CallReasoningLlmProviderOnly
                | Self::GenerateSummary
                | Self::GenerateDistill
                | Self::ExtractMetadata
                | Self::ExpandSearchKeywords
                | Self::ExtractFacts
        )
    }

    fn drops_receipt_for_covered_1522(self) -> bool {
        self == Self::RecordCall || self.is_text_only_model_api()
    }

    fn is_receipt_bearing(self) -> bool {
        matches!(
            self,
            Self::RecordCallWithReceipt
                | Self::CallExtractLlmWithReceipt
                | Self::CallDistillLlmWithReceipt
                | Self::CallSummaryLlmWithReceipt
                | Self::CallReasoningLlmWithReceipt
                | Self::CallReasoningLlmProviderOnlyWithReceipt
                | Self::GenerateSummaryWithReceipt
                | Self::GenerateDistillWithReceipt
                | Self::ExtractMetadataWithReceipt
                | Self::ExpandSearchKeywordsWithReceipt
                | Self::ExtractFactsWithReceipt
        )
    }

    fn all() -> &'static [Self] {
        &[
            Self::CallReasoningLlmProviderOnlyWithReceipt,
            Self::CallReasoningLlmProviderOnly,
            Self::CallReasoningLlmWithReceipt,
            Self::CallSummaryLlmWithReceipt,
            Self::CallDistillLlmWithReceipt,
            Self::CallExtractLlmWithReceipt,
            Self::RecordCallWithReceipt,
            Self::GenerateSummaryWithReceipt,
            Self::GenerateDistillWithReceipt,
            Self::ExtractMetadataWithReceipt,
            Self::ExpandSearchKeywordsWithReceipt,
            Self::ExtractFactsWithReceipt,
            Self::ExpandSearchKeywords,
            Self::CallReasoningLlm,
            Self::CallSummaryLlm,
            Self::CallDistillLlm,
            Self::CallExtractLlm,
            Self::GenerateSummary,
            Self::GenerateDistill,
            Self::ExtractMetadata,
            Self::ExtractFacts,
            Self::RecordCall,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelCallDisposition {
    Unknown,
    PersistedModelInvocation,
    RecorderRunDirectory,
    RecorderRunDirectoryWithTypedReceipt,
    ResponseAndTrajectoryDurableSink,
    ReadinessResponseOnly,
    RecallCacheEphemeral,
    FoundryRecallPersistenceDisabled,
    ProviderProbeOnly,
    GithubCorpusReceiptLedger,
    BackgroundEnrichment,
    DailyReportArtifact,
    DocsResearchDerivedMetadata,
    ContradictionSupersedeFollowup,
    StaffingTelemetry,
}

impl ModelCallDisposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::PersistedModelInvocation => "persisted_model_invocation",
            Self::RecorderRunDirectory => "recorder_run_directory",
            Self::RecorderRunDirectoryWithTypedReceipt => {
                "recorder_run_directory_with_typed_receipt"
            }
            Self::ResponseAndTrajectoryDurableSink => "response_and_trajectory_durable_sink",
            Self::ReadinessResponseOnly => "readiness_response_only",
            Self::RecallCacheEphemeral => "recall_cache_ephemeral",
            Self::FoundryRecallPersistenceDisabled => "foundry_recall_persistence_disabled",
            Self::ProviderProbeOnly => "provider_probe_only",
            Self::GithubCorpusReceiptLedger => "github_corpus_receipt_ledger",
            Self::BackgroundEnrichment => "background_enrichment",
            Self::DailyReportArtifact => "daily_report_artifact",
            Self::DocsResearchDerivedMetadata => "docs_research_derived_metadata",
            Self::ContradictionSupersedeFollowup => "contradiction_supersede_followup",
            Self::StaffingTelemetry => "staffing_telemetry",
        }
    }

    fn is_unknown(self) -> bool {
        self == Self::Unknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelCallClassification {
    Covered1522,
    Owned1523,
    Owned1524,
    Owned1535,
    Owned1536,
    Owned1537,
    Transient,
    OperationalProbe,
    AlreadyReceiptBearing,
    Unknown,
}

impl ModelCallClassification {
    fn as_str(self) -> &'static str {
        match self {
            Self::Covered1522 => "Covered1522",
            Self::Owned1523 => "Owned1523",
            Self::Owned1524 => "Owned1524",
            Self::Owned1535 => "Owned1535",
            Self::Owned1536 => "Owned1536",
            Self::Owned1537 => "Owned1537",
            Self::Transient => "Transient",
            Self::OperationalProbe => "OperationalProbe",
            Self::AlreadyReceiptBearing => "AlreadyReceiptBearing",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone)]
struct ModelCallRecord {
    slot: &'static str,
    file: &'static str,
    owner: &'static str,
    api: ModelApi,
    successor_api: Option<ModelApi>,
    occurrence: usize,
    note: &'static str,
    disposition: ModelCallDisposition,
    classification: ModelCallClassification,
}

impl ModelCallRecord {
    fn key(&self) -> String {
        self.slot.to_string()
    }

    fn matches(&self, call: &ActualModelCall) -> bool {
        self.file == call.file.as_str()
            && self.owner == call.owner.as_str()
            && self.occurrence == call.occurrence
            && self.accepted_apis().contains(&call.api)
    }

    fn accepted_apis(&self) -> Vec<ModelApi> {
        let mut apis = vec![self.api];
        if let Some(successor_api) = self.successor_api {
            apis.push(successor_api);
        }
        apis
    }

    fn diagnostic(&self) -> String {
        format!(
            "{} file={} owner={} occurrence={} current_api={} successor_api={} note={} disposition={} classification={}",
            self.slot,
            self.file,
            self.owner,
            self.occurrence,
            self.api.as_str(),
            self.successor_api
                .map(ModelApi::as_str)
                .unwrap_or("<none>"),
            self.note,
            self.disposition.as_str(),
            self.classification.as_str()
        )
    }
}

#[derive(Debug, Clone)]
struct ActualModelCall {
    file: String,
    line: usize,
    owner: String,
    api: ModelApi,
    occurrence: usize,
    snippet: String,
}

impl ActualModelCall {
    fn key(&self) -> String {
        model_call_key(&self.file, &self.owner, self.api, self.occurrence)
    }

    fn diagnostic(&self) -> String {
        format!(
            "{}:{} owner={} api={} occurrence={} snippet={}",
            self.file,
            self.line,
            self.owner,
            self.api.as_str(),
            self.occurrence,
            self.snippet
        )
    }
}

fn model_call_key(file: &str, owner: &str, api: ModelApi, occurrence: usize) -> String {
    format!("{}|{}|{}|{}", file, owner, api.as_str(), occurrence)
}

fn record(
    slot: &'static str,
    file: &'static str,
    owner: &'static str,
    api: ModelApi,
    occurrence: usize,
    note: &'static str,
    disposition: ModelCallDisposition,
    classification: ModelCallClassification,
) -> ModelCallRecord {
    ModelCallRecord {
        slot,
        file,
        owner,
        api,
        successor_api: None,
        occurrence,
        note,
        disposition,
        classification,
    }
}

fn record_with_successor(
    slot: &'static str,
    file: &'static str,
    owner: &'static str,
    api: ModelApi,
    successor_api: ModelApi,
    occurrence: usize,
    note: &'static str,
    disposition: ModelCallDisposition,
    classification: ModelCallClassification,
) -> ModelCallRecord {
    ModelCallRecord {
        slot,
        file,
        owner,
        api,
        successor_api: Some(successor_api),
        occurrence,
        note,
        disposition,
        classification,
    }
}

fn model_call_registry() -> Vec<ModelCallRecord> {
    use ModelApi::*;
    use ModelCallClassification::*;
    use ModelCallDisposition::*;
    vec![
        record("github_corpus_pilot_generate_provider_receipt", "bin/github_corpus_pilot.rs", "generate", CallReasoningLlmProviderOnlyWithReceipt, 1, "already-receipted GitHub corpus pilot ledger", GithubCorpusReceiptLedger, AlreadyReceiptBearing),
        record_with_successor("bootstrap_backfill_generate_summary", "bootstrap/backfill.rs", "generate_summary_with_retry", GenerateSummary, GenerateSummaryWithReceipt, 1, "legacy metadata backfill summary", BackgroundEnrichment, Owned1523),
        record_with_successor("bootstrap_backfill_extract_metadata", "bootstrap/backfill.rs", "extract_metadata_with_retry", ExtractMetadata, ExtractMetadataWithReceipt, 1, "legacy metadata backfill extraction", BackgroundEnrichment, Owned1523),
        record("continuity_pipeline_candidate_distill_receipt", "continuity_ops/pipeline.rs", "maybe_spawn_session_continuity_pipeline", CallDistillLlmWithReceipt, 1, "continuity candidate event provenance", PersistedModelInvocation, Covered1522),
        record("continuity_pipeline_outcome_reasoning_receipt", "continuity_ops/pipeline.rs", "maybe_spawn_session_continuity_pipeline", CallReasoningLlmWithReceipt, 1, "continuity outcome label provenance", PersistedModelInvocation, Covered1522),
        record("daily_health_reasoning", "daily_pipeline/health.rs", "run_health_check", CallReasoningLlmWithReceipt, 1, "daily health report artifact + model-invocations-v1 sidecar + health-only wiki", DailyReportArtifact, Covered1522),
        record("daily_routing_reasoning", "daily_pipeline/routing.rs", "run_routing_analysis_stage", CallReasoningLlmWithReceipt, 1, "daily routing report artifact + model-invocations-v1 sidecar", DailyReportArtifact, Covered1522),
        record_with_successor("dispatch_plan_recorder", "dispatch_ops/dispatch_v2.rs", "call_plan_llm", RecordCall, RecordCallWithReceipt, 1, "dispatch plan run artifact recorder", RecorderRunDirectory, Owned1536),
        record_with_successor("dispatch_plan_provider_call", "dispatch_ops/dispatch_v2.rs", "call_plan_llm", CallReasoningLlmProviderOnly, CallReasoningLlmProviderOnlyWithReceipt, 1, "dispatch plan model call inside recorder closure", RecorderRunDirectory, Owned1536),
        record_with_successor("docs_classify_extract_metadata", "docs_ops/classify.rs", "classify_and_extract_metadata", CallExtractLlm, CallExtractLlmWithReceipt, 1, "docs organize filesystem mutation metadata", DocsResearchDerivedMetadata, Owned1537),
        record_with_successor("enrichment_flush_generate_summary", "enrichment.rs", "flush_enrichment_batch", GenerateSummary, GenerateSummaryWithReceipt, 1, "background enrichment summary", BackgroundEnrichment, Owned1523),
        record_with_successor("enrichment_flush_extract_metadata", "enrichment.rs", "flush_enrichment_batch", ExtractMetadata, ExtractMetadataWithReceipt, 1, "background enrichment metadata", BackgroundEnrichment, Owned1523),
        record_with_successor("enrichment_flush_expand_keywords", "enrichment.rs", "flush_enrichment_batch", ExpandSearchKeywords, ExpandSearchKeywordsWithReceipt, 1, "background keyword enrichment", BackgroundEnrichment, Owned1523),
        record("readiness_synthesize_answer_reasoning_receipt", "facade_memory_ops/readiness_ops.rs", "synthesize_answer", CallReasoningLlmWithReceipt, 1, "readiness response-only synthesis", ReadinessResponseOnly, Transient),
        record("daily_distill_claude_batch_recorder_receipt", "foundry_runtime_ops/daily_distill/runner.rs", "call_claude_batch", RecordCallWithReceipt, 1, "daily distill recorder with typed receipt", RecorderRunDirectoryWithTypedReceipt, Covered1522),
        record("daily_distill_claude_batch_model_receipt", "foundry_runtime_ops/daily_distill/runner.rs", "call_claude_batch", CallDistillLlmWithReceipt, 1, "daily distill model call inside recorder closure", PersistedModelInvocation, Covered1522),
        record("daily_distill_api_batch_model_receipt", "foundry_runtime_ops/daily_distill/runner.rs", "call_api_batch_distill", CallDistillLlmWithReceipt, 1, "daily distill raw API batch", PersistedModelInvocation, Covered1522),
        record("daily_distill_fallback_model_receipt", "foundry_runtime_ops/daily_distill/runner.rs", "fallback_distill_with_receipt", CallDistillLlmWithReceipt, 1, "daily distill fallback", PersistedModelInvocation, Covered1522),
        record("capture_session_extract_receipt", "foundry_runtime_ops/handlers/capture_session.rs", "handle_capture_session", CallExtractLlmWithReceipt, 1, "capture session durable drafts", PersistedModelInvocation, Covered1522),
        record("maintenance_distill_job_generate_receipt", "foundry_runtime_ops/maintenance/distill_job.rs", "process_memory_distill_job", GenerateDistillWithReceipt, 1, "maintenance guide distill", PersistedModelInvocation, Covered1522),
        record("foundry_recall_compaction_extract", "foundry_runtime_ops/recall.rs", "run_compaction_model", CallExtractLlm, 1, "foundry recall persistence disabled", FoundryRecallPersistenceDisabled, Transient),
        record("recall_cache_query_extract", "foundry_runtime_ops/recall_cache/queries.rs", "generate_recall_cache_query_via_llm", CallExtractLlm, 1, "ephemeral recall_cache query", RecallCacheEphemeral, Transient),
        record("wiki_evolver_draft_distill_receipt", "foundry_runtime_ops/wiki_evolver.rs", "synthesize_wiki_draft", CallDistillLlmWithReceipt, 1, "REM wiki draft durable facade write", PersistedModelInvocation, Covered1522),
        record("hub_call_execute_skill_extract_receipt", "hub_ops/call.rs", "execute_skill_prompt_with_receipt", CallExtractLlmWithReceipt, 1, "normal hub_call response plus trajectory distill durable snapshot/capability provenance sink", ResponseAndTrajectoryDurableSink, Covered1522),
        record_with_successor("hub_register_recorder", "hub_ops/register.rs", "handle_hub_register", RecordCall, RecordCallWithReceipt, 1, "hub capability register recorder", RecorderRunDirectory, Owned1535),
        record_with_successor("hub_register_extract", "hub_ops/register.rs", "handle_hub_register", CallExtractLlm, CallExtractLlmWithReceipt, 1, "hub capability register model call inside recorder closure", RecorderRunDirectory, Owned1535),
        record_with_successor("hub_security_scan_extract", "hub_ops/security_scan/llm_scan.rs", "scan_skill_definition_with_llm", CallExtractLlm, CallExtractLlmWithReceipt, 1, "hub capability security scan weak-tier call", StaffingTelemetry, Owned1535),
        record_with_successor("hub_security_vote_recorder", "hub_ops/security_scan/llm_scan.rs", "one_vote", RecordCall, RecordCallWithReceipt, 1, "hub capability security scan vote recorder", RecorderRunDirectory, Owned1535),
        record_with_successor("hub_security_vote_provider_call", "hub_ops/security_scan/llm_scan.rs", "one_vote", CallReasoningLlmProviderOnly, CallReasoningLlmProviderOnlyWithReceipt, 1, "hub capability security scan vote model call inside recorder closure", RecorderRunDirectory, Owned1535),
        record_with_successor("contradiction_verify_extract", "memory_search_ops/contradiction.rs", "verify_contradiction_candidate", CallExtractLlm, CallExtractLlmWithReceipt, 1, "contradiction supersede follow-up", ContradictionSupersedeFollowup, Owned1524),
        record("ingest_event_extract_facts_receipt", "pipeline_ops/ingest/event.rs", "handle_ingest_event", ExtractFactsWithReceipt, 1, "event fact extraction durable facts", PersistedModelInvocation, Covered1522),
        record("ingest_extract_facts_receipt", "pipeline_ops/ingest/extract.rs", "handle_extract_facts", ExtractFactsWithReceipt, 1, "standalone fact extraction durable facts", PersistedModelInvocation, Covered1522),
        record_with_successor("research_digest_extract", "research_ops.rs", "build_digest", CallExtractLlm, CallExtractLlmWithReceipt, 1, "research digest artifact", DocsResearchDerivedMetadata, Owned1536),
        record("provider_probe_report_extract", "status_ops/status_health/probes.rs", "run_provider_probe_report_with_migration_authority", CallExtractLlm, 1, "provider probe cache extract", ProviderProbeOnly, OperationalProbe),
        record("provider_probe_report_distill", "status_ops/status_health/probes.rs", "run_provider_probe_report_with_migration_authority", CallDistillLlm, 1, "provider probe cache distill", ProviderProbeOnly, OperationalProbe),
        record("provider_rotation_probe_extract", "status_ops/status_health/probes.rs", "probe_rotation_member", CallExtractLlm, 1, "provider rotation probe extract", ProviderProbeOnly, OperationalProbe),
        record("provider_rotation_probe_distill", "status_ops/status_health/probes.rs", "probe_rotation_member", CallDistillLlm, 1, "provider rotation probe distill", ProviderProbeOnly, OperationalProbe),
        record("wiki_ingest_extract_receipt", "wiki_ops/ingest.rs", "extract_ingest_metadata", CallExtractLlmWithReceipt, 1, "wiki ingest durable metadata", PersistedModelInvocation, Covered1522),
        record("workflow_closure_draft_distill_receipt", "workflow_closure.rs", "draft_from_result", GenerateDistillWithReceipt, 1, "workflow closure wiki draft durable facade write", PersistedModelInvocation, Covered1522),
    ]
}

fn discover_production_model_calls() -> Vec<ActualModelCall> {
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rust_sources(&source_root, &mut files);
    files.sort();

    let mut calls = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&source_root)
            .expect("source file under crate src")
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_only_source_path(&rel) {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read production Rust source");
        calls.extend(discover_model_calls_in_source(&rel, &source));
    }
    calls
}

fn discover_model_calls_in_source(file: &str, source: &str) -> Vec<ActualModelCall> {
    let lines = strip_test_only_source(source);
    let surface_lines = lexical_surface(&lines.join("\n"));
    let mut raw_calls = Vec::<(usize, String, ModelApi, String)>::new();
    for (idx, api) in detect_model_api_calls(&surface_lines) {
        raw_calls.push((
            idx + 1,
            enclosing_owner(&surface_lines, idx),
            api,
            call_snippet(&lines, idx),
        ));
    }

    let mut occurrences = BTreeMap::<(String, ModelApi), usize>::new();
    raw_calls
        .into_iter()
        .map(|(line, owner, api, snippet)| {
            let occurrence = occurrences.entry((owner.clone(), api)).or_default();
            *occurrence += 1;
            ActualModelCall {
                file: file.to_string(),
                line,
                owner,
                api,
                occurrence: *occurrence,
                snippet,
            }
        })
        .collect()
}

fn collect_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read source directory") {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            collect_rust_sources(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn is_test_only_source_path(rel: &str) -> bool {
    rel.split('/').any(|part| part == "tests")
        || rel.ends_with("/tests.rs")
        || rel == "tests.rs"
        || rel.ends_with("_tests.rs")
}

fn strip_test_only_source(source: &str) -> Vec<String> {
    let surface = lexical_surface(source);
    let original = source.lines().map(str::to_string).collect::<Vec<_>>();
    let mut out = Vec::new();
    let mut pending_cfg_test = false;
    let mut skipping = false;
    let mut seen_brace = false;
    let mut brace_depth: isize = 0;
    for (idx, line) in original.iter().enumerate() {
        let surface_line = surface.get(idx).map(String::as_str).unwrap_or_default();
        let trimmed = surface_line.trim();
        if skipping {
            let delta = brace_delta_surface(surface_line);
            if delta > 0 {
                seen_brace = true;
            }
            brace_depth += delta;
            out.push(String::new());
            if (seen_brace && brace_depth <= 0) || (!seen_brace && trimmed.ends_with(';')) {
                skipping = false;
                pending_cfg_test = false;
                seen_brace = false;
            }
            continue;
        }
        if is_cfg_test_attr(trimmed) {
            pending_cfg_test = true;
            out.push(String::new());
            continue;
        }
        if pending_cfg_test {
            out.push(String::new());
            if trimmed.is_empty() || trimmed.starts_with("#[") {
                continue;
            }
            let delta = brace_delta_surface(surface_line);
            seen_brace = delta > 0;
            brace_depth = delta;
            if (seen_brace && brace_depth <= 0) || (!seen_brace && trimmed.ends_with(';')) {
                pending_cfg_test = false;
                seen_brace = false;
            } else {
                skipping = true;
            }
            continue;
        }
        out.push(line.clone());
    }
    out
}

fn is_cfg_test_attr(trimmed_surface_line: &str) -> bool {
    let compact = trimmed_surface_line.split_whitespace().collect::<String>();
    compact.starts_with("#[cfg(test)]") || compact.starts_with("#[cfg(any(test")
}

fn brace_delta_surface(line: &str) -> isize {
    line.chars().filter(|ch| *ch == '{').count() as isize
        - line.chars().filter(|ch| *ch == '}').count() as isize
}

fn detect_model_api_calls(surface_lines: &[String]) -> Vec<(usize, ModelApi)> {
    let surface = surface_lines.join("\n");
    let mut line_starts = vec![0usize];
    for (idx, byte) in surface.bytes().enumerate() {
        if byte == b'\n' {
            line_starts.push(idx + 1);
        }
    }

    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < surface.len() {
        let rest = &surface[offset..];
        let prefix_len = if rest.starts_with('.') {
            Some(1)
        } else if rest.starts_with("::") {
            Some(2)
        } else {
            None
        };
        let mut matched = None;
        if let Some(prefix_len) = prefix_len {
            let after_prefix = &rest[prefix_len..];
            for api in ModelApi::all().iter().copied() {
                let name = api.as_str();
                if after_prefix.starts_with(name)
                    && model_api_call_has_open_paren(after_prefix, name.len())
                {
                    matched = Some((api, prefix_len + name.len()));
                    break;
                }
            }
        }
        if let Some((api, len)) = matched {
            let line = match line_starts.binary_search(&offset) {
                Ok(index) => index,
                Err(index) => index.saturating_sub(1),
            };
            out.push((line, api));
            offset += len;
        } else {
            offset += rest.chars().next().map(char::len_utf8).unwrap_or(1);
        }
    }
    out
}

fn model_api_call_has_open_paren(rest: &str, call_len: usize) -> bool {
    rest[call_len..].trim_start().starts_with('(')
}

fn lexical_surface(source: &str) -> Vec<String> {
    let chars = source.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(source.len());
    let mut idx = 0usize;
    let mut state = LexState::Code;
    while idx < chars.len() {
        let ch = chars[idx];
        match state {
            LexState::Code => {
                if ch == '/' && chars.get(idx + 1) == Some(&'/') {
                    out.push(' ');
                    out.push(' ');
                    idx += 2;
                    state = LexState::LineComment;
                    continue;
                }
                if ch == '/' && chars.get(idx + 1) == Some(&'*') {
                    out.push(' ');
                    out.push(' ');
                    idx += 2;
                    state = LexState::BlockComment { depth: 1 };
                    continue;
                }
                if let Some(hash_count) = raw_string_hash_count(&chars, idx) {
                    for _ in 0..(2 + hash_count) {
                        out.push(' ');
                    }
                    idx += 2 + hash_count;
                    state = LexState::RawString { hash_count };
                    continue;
                }
                if ch == '"' {
                    out.push(' ');
                    idx += 1;
                    state = LexState::String { escaped: false };
                    continue;
                }
                if ch == '\'' {
                    out.push(' ');
                    idx += 1;
                    state = LexState::Char { escaped: false };
                    continue;
                }
                out.push(ch);
                idx += 1;
            }
            LexState::LineComment => {
                if ch == '\n' {
                    out.push('\n');
                    state = LexState::Code;
                } else {
                    out.push(' ');
                }
                idx += 1;
            }
            LexState::BlockComment { mut depth } => {
                if ch == '/' && chars.get(idx + 1) == Some(&'*') {
                    out.push(' ');
                    out.push(' ');
                    idx += 2;
                    depth += 1;
                    state = LexState::BlockComment { depth };
                } else if ch == '*' && chars.get(idx + 1) == Some(&'/') {
                    out.push(' ');
                    out.push(' ');
                    idx += 2;
                    depth -= 1;
                    state = if depth == 0 {
                        LexState::Code
                    } else {
                        LexState::BlockComment { depth }
                    };
                } else {
                    out.push(if ch == '\n' { '\n' } else { ' ' });
                    idx += 1;
                }
            }
            LexState::String { escaped } => {
                if ch == '\n' {
                    out.push('\n');
                    idx += 1;
                    state = LexState::String { escaped: false };
                } else if escaped {
                    out.push(' ');
                    idx += 1;
                    state = LexState::String { escaped: false };
                } else if ch == '\\' {
                    out.push(' ');
                    idx += 1;
                    state = LexState::String { escaped: true };
                } else if ch == '"' {
                    out.push(' ');
                    idx += 1;
                    state = LexState::Code;
                } else {
                    out.push(' ');
                    idx += 1;
                }
            }
            LexState::Char { escaped } => {
                if ch == '\n' {
                    out.push('\n');
                    idx += 1;
                    state = LexState::Code;
                } else if escaped {
                    out.push(' ');
                    idx += 1;
                    state = LexState::Char { escaped: false };
                } else if ch == '\\' {
                    out.push(' ');
                    idx += 1;
                    state = LexState::Char { escaped: true };
                } else if ch == '\'' {
                    out.push(' ');
                    idx += 1;
                    state = LexState::Code;
                } else {
                    out.push(' ');
                    idx += 1;
                }
            }
            LexState::RawString { hash_count } => {
                if ch == '"' && raw_string_closes(&chars, idx, hash_count) {
                    for _ in 0..(1 + hash_count) {
                        out.push(' ');
                    }
                    idx += 1 + hash_count;
                    state = LexState::Code;
                } else {
                    out.push(if ch == '\n' { '\n' } else { ' ' });
                    idx += 1;
                }
            }
        }
    }
    out.lines().map(str::to_string).collect()
}

#[derive(Debug, Clone, Copy)]
enum LexState {
    Code,
    LineComment,
    BlockComment { depth: usize },
    String { escaped: bool },
    Char { escaped: bool },
    RawString { hash_count: usize },
}

fn raw_string_hash_count(chars: &[char], idx: usize) -> Option<usize> {
    if chars.get(idx) != Some(&'r') {
        return None;
    }
    let mut cursor = idx + 1;
    let mut hash_count = 0;
    while chars.get(cursor) == Some(&'#') {
        hash_count += 1;
        cursor += 1;
    }
    (chars.get(cursor) == Some(&'"')).then_some(hash_count)
}

fn raw_string_closes(chars: &[char], idx: usize, hash_count: usize) -> bool {
    chars.get(idx) == Some(&'"')
        && (0..hash_count).all(|offset| chars.get(idx + 1 + offset) == Some(&'#'))
}

fn call_snippet(lines: &[String], idx: usize) -> String {
    lines
        .iter()
        .take((idx + 4).min(lines.len()))
        .skip(idx)
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn enclosing_owner(lines: &[String], call_index: usize) -> String {
    lines[..=call_index]
        .iter()
        .rev()
        .find_map(|line| parse_fn_name(line))
        .unwrap_or_else(|| "<module>".to_string())
}

fn parse_fn_name(line: &str) -> Option<String> {
    let code = line.split("//").next()?.trim();
    let fn_pos = code.find("fn ")?;
    let after = &code[fn_pos + 3..];
    let name = after
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect::<String>();
    (!name.is_empty()).then_some(name)
}

fn unregistered_model_calls(
    actual: &[ActualModelCall],
    registry: &[ModelCallRecord],
) -> Vec<String> {
    validate_model_call_census(actual, registry)
        .into_iter()
        .filter(|failure| failure.contains("missing typed registry slot entries"))
        .collect()
}

fn synthetic_successor_slot_registry() -> Vec<ModelCallRecord> {
    vec![record_with_successor(
        "synthetic_summary_slot",
        "synthetic.rs",
        "production",
        ModelApi::GenerateSummary,
        ModelApi::GenerateSummaryWithReceipt,
        1,
        "synthetic Owned1523 summary slot",
        ModelCallDisposition::BackgroundEnrichment,
        ModelCallClassification::Owned1523,
    )]
}

fn synthetic_census_errors(source: &str, registry: &[ModelCallRecord]) -> String {
    let actual = discover_model_calls_in_source("synthetic.rs", source);
    validate_model_call_census(&actual, registry).join("\n\n")
}

#[test]
fn model_call_scanner_strips_embedded_cfg_test_module_and_multiline_cfg_test_fn() {
    let actual = discover_model_calls_in_source(
        "synthetic.rs",
        r#"
async fn production(server: &Server) {
    server.llm.call_extract_llm("system", "user", None, 0.0, 8).await;
}

#[cfg(test)]
mod tests {
    async fn helper(server: &Server) {
        server.llm.call_distill_llm("system", "user", None, 0.0, 8).await;
    }
}

#[cfg(test)]
async fn multiline_test_only(
    server: &Server,
) {
    server.llm.call_reasoning_llm("system", "user", None, 0.0, 8).await;
}
"#,
    );
    assert_eq!(actual.len(), 1, "{actual:#?}");
    assert_eq!(actual[0].owner, "production");
    assert_eq!(actual[0].api, ModelApi::CallExtractLlm);
}

#[test]
fn model_call_scanner_ignores_braces_in_strings_and_comments_while_stripping_cfg_test() {
    let actual = discover_model_calls_in_source(
        "synthetic.rs",
        r#"
#[cfg(test)]
async fn test_only(server: &Server) {
    let _json = "{\"brace\":\"}\"}";
    // }}} comments must not terminate the cfg(test) item
    server.llm.call_distill_llm("system", "user", None, 0.0, 8).await;
}

async fn production(server: &Server) {
    server.llm.call_extract_llm("system", "user", None, 0.0, 8).await;
}
"#,
    );
    assert_eq!(actual.len(), 1, "{actual:#?}");
    assert_eq!(actual[0].owner, "production");
    assert_eq!(actual[0].api, ModelApi::CallExtractLlm);
}

#[test]
fn model_call_scanner_detects_multiline_calls_and_recorder_closure_inner_calls() {
    let actual = discover_model_calls_in_source(
        "synthetic.rs",
        r#"
async fn production(server: &Server, llm: LlmClient, prompt: String) {
    server.llm.call_summary_llm
        ("system", "user", None, 0.0, 8)
        .await;
    server.llm_recorder.record_call("label", &prompt, move || async move {
        llm.generate_summary_with_receipt
            (&prompt)
            .await
    }).await;
}
"#,
    );
    let apis = actual.iter().map(|call| call.api).collect::<Vec<_>>();
    assert_eq!(
        apis,
        vec![
            ModelApi::CallSummaryLlm,
            ModelApi::RecordCall,
            ModelApi::GenerateSummaryWithReceipt,
        ],
        "{actual:#?}"
    );
    assert!(
        actual
            .iter()
            .any(|call| call.owner == "production"
                && call.api == ModelApi::GenerateSummaryWithReceipt),
        "recorder closure inner call must be discovered: {actual:#?}"
    );
}

#[test]
fn model_call_scanner_collects_two_calls_on_one_line() {
    let actual = discover_model_calls_in_source(
        "synthetic.rs",
        r#"
async fn production(server: &Server, llm: LlmClient) {
    server.llm.call_extract_llm("system", "user", None, 0.0, 8).await; llm.generate_summary("text").await;
}
"#,
    );
    let apis = actual.iter().map(|call| call.api).collect::<Vec<_>>();
    assert_eq!(
        apis,
        vec![ModelApi::CallExtractLlm, ModelApi::GenerateSummary],
        "{actual:#?}"
    );
}

#[test]
fn model_call_scanner_detects_ufcs_calls_and_preserves_multicall_order() {
    let actual = discover_model_calls_in_source(
        "synthetic.rs",
        r#"
async fn production(llm: &LlmClient) {
    LlmClient::call_extract_llm(llm, "system", "user", None, 0.0, 8).await; llm.generate_summary("text").await;
}
"#,
    );
    let apis = actual.iter().map(|call| call.api).collect::<Vec<_>>();
    assert_eq!(
        apis,
        vec![ModelApi::CallExtractLlm, ModelApi::GenerateSummary],
        "{actual:#?}"
    );
    assert_eq!(actual[0].owner, "production");
}

#[test]
fn model_call_scanner_rejects_similar_names_and_function_values() {
    let actual = discover_model_calls_in_source(
        "synthetic.rs",
        r#"
async fn production(llm: &LlmClient) {
    let _function = llm.call_extract_llm;
    llm.call_extract_llm_suffix("not the registered API").await;
    llm.call_extract_llm /* comments are blanked by the lexical surface */
        ("system", "user", None, 0.0, 8).await;
}
"#,
    );
    assert_eq!(actual.len(), 1, "{actual:#?}");
    assert_eq!(actual[0].api, ModelApi::CallExtractLlm);
}

#[test]
fn model_call_scanner_ignores_block_comments_and_multiline_raw_strings() {
    let actual = discover_model_calls_in_source(
        "synthetic.rs",
        r##"
async fn production(server: &Server) {
    /*
       server.llm.call_summary_llm("fake", "fake", None, 0.0, 8).await;
    */
    let _fake = r#"
       llm.generate_distill_with_receipt("fake").await;
       llm.extract_metadata_with_receipt("fake").await;
    "#;
    server.llm.call_extract_llm("system", "user", None, 0.0, 8).await;
}
"##,
    );
    assert_eq!(actual.len(), 1, "{actual:#?}");
    assert_eq!(actual[0].api, ModelApi::CallExtractLlm);
}

#[test]
fn model_call_guard_fails_an_unregistered_currently_zero_receipt_api_call() {
    let actual = discover_model_calls_in_source(
        "synthetic.rs",
        r#"
async fn production(llm: LlmClient) {
    llm.generate_summary_with_receipt("durable summary").await.unwrap();
}
"#,
    );
    let failures = unregistered_model_calls(&actual, &[]);
    assert_eq!(failures.len(), 1, "{failures:#?}");
    assert!(
        failures[0].contains("api=generate_summary_with_receipt"),
        "unregistered currently-zero receipt API must fail with a useful diagnostic: {failures:#?}"
    );
}

#[test]
fn model_call_registry_accepts_standalone_text_state_for_successor_slot() {
    let errors = synthetic_census_errors(
        r#"
async fn production(llm: LlmClient) {
    llm.generate_summary("durable summary").await.unwrap();
}
"#,
        &synthetic_successor_slot_registry(),
    );
    assert!(errors.is_empty(), "{errors}");
}

#[test]
fn model_call_registry_accepts_exact_receipt_successor_state() {
    let errors = synthetic_census_errors(
        r#"
async fn production(llm: LlmClient) {
    llm.generate_summary_with_receipt("durable summary").await.unwrap();
}
"#,
        &synthetic_successor_slot_registry(),
    );
    assert!(errors.is_empty(), "{errors}");
}

#[test]
fn model_call_registry_matches_registered_slot_using_ufcs_call() {
    let errors = synthetic_census_errors(
        r#"
async fn production(llm: &LlmClient) {
    LlmClient::generate_summary(llm, "durable summary").await.unwrap();
}
"#,
        &synthetic_successor_slot_registry(),
    );
    assert!(errors.is_empty(), "{errors}");
}

#[test]
fn model_call_registry_rejects_current_and_successor_together() {
    let errors = synthetic_census_errors(
        r#"
async fn production(llm: LlmClient) {
    llm.generate_summary("durable summary").await.unwrap();
    llm.generate_summary_with_receipt("durable summary").await.unwrap();
}
"#,
        &synthetic_successor_slot_registry(),
    );
    assert!(
        errors.contains("matched multiple production callsites")
            && errors.contains("generate_summary_with_receipt"),
        "current plus successor must fail as a duplicate slot match: {errors}"
    );
}

#[test]
fn model_call_registry_rejects_extra_unknown_call() {
    let errors = synthetic_census_errors(
        r#"
async fn production(llm: LlmClient) {
    llm.generate_summary("durable summary").await.unwrap();
    llm.call_extract_llm("system", "user", None, 0.0, 8).await.unwrap();
}
"#,
        &synthetic_successor_slot_registry(),
    );
    assert!(
        errors.contains("missing typed registry slot entries")
            && errors.contains("api=call_extract_llm"),
        "extra unknown call must fail with the actual API diagnostic: {errors}"
    );
}

#[test]
fn model_call_registry_rejects_unregistered_ufcs_call() {
    let errors = synthetic_census_errors(
        r#"
async fn production(llm: &LlmClient) {
    LlmClient::call_extract_llm(llm, "system", "user", None, 0.0, 8).await.unwrap();
}
"#,
        &synthetic_successor_slot_registry(),
    );
    assert!(
        errors.contains("missing typed registry slot entries")
            && errors.contains("api=call_extract_llm"),
        "unregistered UFCS call must fail with the actual API diagnostic: {errors}"
    );
}

#[test]
fn model_call_registry_rejects_wrong_successor_api() {
    let errors = synthetic_census_errors(
        r#"
async fn production(llm: LlmClient) {
    llm.extract_metadata_with_receipt("durable summary").await.unwrap();
}
"#,
        &synthetic_successor_slot_registry(),
    );
    assert!(
        errors.contains("api=extract_metadata_with_receipt")
            && errors.contains("typed model-call registry slots no longer match"),
        "wrong receipt-bearing sibling must not satisfy the slot: {errors}"
    );
}

#[test]
fn covered_1522_rejects_record_call_as_receipt_dropping_wrapper() {
    let record = record(
        "synthetic_covered_record_call",
        "synthetic.rs",
        "durable_owner",
        ModelApi::RecordCall,
        1,
        "synthetic durable regression from record_call_with_receipt to record_call",
        ModelCallDisposition::RecorderRunDirectory,
        ModelCallClassification::Covered1522,
    );
    assert!(
        record.api.drops_receipt_for_covered_1522(),
        "Covered1522 must reject record_call because it drops typed receipts"
    );
}

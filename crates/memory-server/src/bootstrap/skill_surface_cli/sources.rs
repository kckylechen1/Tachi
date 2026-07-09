use super::*;

pub(super) fn build_skill_source_report() -> Result<SkillSourceReport, String> {
    let mut summary = SkillSourceReportSummary {
        corpora: SKILL_SOURCE_MANIFESTS.len(),
        ..SkillSourceReportSummary::default()
    };
    let mut corpora = Vec::new();

    for spec in SKILL_SOURCE_MANIFESTS {
        let content = read_skill_source_manifest_content(spec)?;
        let parsed = parse_skill_source_manifest(&content)
            .map_err(|e| format!("parse {}: {e}", spec.path))?;
        let corpus = build_skill_source_corpus_status(spec, parsed);
        accumulate_source_summary(&mut summary, &corpus.summary);
        corpora.push(corpus);
    }

    Ok(SkillSourceReport {
        schema_version: "tachi.skill_surface.sources.v1".to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        summary,
        corpora,
    })
}

fn build_skill_source_corpus_status(
    spec: &SkillSourceManifestSpec,
    parsed: ParsedSkillSourceManifest,
) -> SkillSourceCorpusStatus {
    let mut summary = SkillSourceReportSummary {
        corpora: 1,
        skills: parsed.skills.len(),
        ..SkillSourceReportSummary::default()
    };
    let skills: Vec<SkillSourceSkillStatus> = parsed
        .skills
        .into_iter()
        .map(|entry| {
            let metadata_status = skill_source_metadata_status(&entry.source);
            match entry.source.kind.as_deref() {
                Some("upstream_skill_repo") => summary.upstream_managed += 1,
                Some("tachi_native_contract") => summary.native_contracts += 1,
                _ => {}
            }
            if entry.source.update_policy.as_deref() == Some("local_review") {
                summary.local_review += 1;
            }
            if metadata_status == "missing_metadata" {
                summary.missing_metadata += 1;
            }
            SkillSourceSkillStatus {
                id: entry.id,
                name: entry.name,
                local_path: entry.local_path,
                source: entry.source,
                metadata_status,
                latest_status: SOURCE_STATUS_NOT_CHECKED.to_string(),
                diff_status: SOURCE_STATUS_NOT_CHECKED.to_string(),
            }
        })
        .collect();

    SkillSourceCorpusStatus {
        corpus: if parsed.corpus.is_empty() {
            spec.corpus.to_string()
        } else {
            parsed.corpus
        },
        manifest_path: spec.path.to_string(),
        updated: parsed.updated,
        upstream: SkillSourceUpstreamStatus {
            repo: parsed.upstream.repo,
            pinned_ref: parsed.upstream.pinned_ref,
            pinned_sha: parsed.upstream.pinned_sha,
            update_policy: parsed.upstream.update_policy,
            latest_status: SOURCE_STATUS_NOT_CHECKED.to_string(),
            diff_status: SOURCE_STATUS_NOT_CHECKED.to_string(),
            status_reason: SOURCE_STATUS_NOT_CHECKED_REASON.to_string(),
        },
        skills,
        summary,
    }
}

fn accumulate_source_summary(
    total: &mut SkillSourceReportSummary,
    next: &SkillSourceReportSummary,
) {
    total.skills += next.skills;
    total.upstream_managed += next.upstream_managed;
    total.native_contracts += next.native_contracts;
    total.local_review += next.local_review;
    total.missing_metadata += next.missing_metadata;
}

pub(super) fn skill_source_metadata_status(source: &SkillSourceMetadata) -> String {
    match source.kind.as_deref() {
        Some("upstream_skill_repo") => {
            if source.repo.is_some()
                && source.path.is_some()
                && source.pinned_ref.is_some()
                && source.pinned_sha.is_some()
                && source.update_policy.is_some()
            {
                "pinned_upstream".to_string()
            } else {
                "missing_metadata".to_string()
            }
        }
        Some("tachi_native_contract") => {
            if source.update_policy.is_some() {
                "native_contract".to_string()
            } else {
                "missing_metadata".to_string()
            }
        }
        Some(_) => {
            if source.update_policy.is_some() {
                "local_or_external".to_string()
            } else {
                "missing_metadata".to_string()
            }
        }
        None => "missing_metadata".to_string(),
    }
}

pub(super) fn parse_skill_source_manifest(
    content: &str,
) -> Result<ParsedSkillSourceManifest, String> {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Section {
        Top,
        Upstream,
        Skills,
        SkillSource,
    }

    let mut section = Section::Top;
    let mut manifest = ParsedSkillSourceManifest::default();
    let mut current: Option<ParsedSkillSourceEntry> = None;

    for raw in content.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if !line.starts_with(' ') {
            if let Some(entry) = current.take() {
                manifest.skills.push(entry);
            }
            section = match trimmed {
                "upstream:" => Section::Upstream,
                "skills:" => Section::Skills,
                _ => Section::Top,
            };

            if let Some((key, value)) = yaml_key_value(trimmed) {
                match key {
                    "updated" => manifest.updated = value,
                    "corpus" => manifest.corpus = value.unwrap_or_default(),
                    _ => {}
                }
            }
            continue;
        }

        if line.starts_with("  - id:") && matches!(section, Section::Skills | Section::SkillSource)
        {
            if let Some(entry) = current.take() {
                manifest.skills.push(entry);
            }
            let mut entry = ParsedSkillSourceEntry::default();
            entry.id = yaml_scalar(line.trim_start_matches("  - id:").trim()).unwrap_or_default();
            current = Some(entry);
            section = Section::Skills;
            continue;
        }

        match section {
            Section::Upstream => {
                if let Some((key, value)) = yaml_key_value(trimmed) {
                    assign_skill_source_field(&mut manifest.upstream, key, value);
                }
            }
            Section::Skills => {
                if trimmed == "source:" {
                    section = Section::SkillSource;
                    continue;
                }
                if let Some(entry) = current.as_mut() {
                    if let Some((key, value)) = yaml_key_value(trimmed) {
                        match key {
                            "name" => entry.name = value.unwrap_or_default(),
                            "local_path" => entry.local_path = value.unwrap_or_default(),
                            _ => {}
                        }
                    }
                }
            }
            Section::SkillSource => {
                if let Some(entry) = current.as_mut() {
                    if let Some((key, value)) = yaml_key_value(trimmed) {
                        assign_skill_source_field(&mut entry.source, key, value);
                    }
                }
            }
            Section::Top => {}
        }
    }

    if let Some(entry) = current {
        manifest.skills.push(entry);
    }

    if manifest.corpus.is_empty() {
        return Err("missing corpus".to_string());
    }
    if manifest.skills.is_empty() {
        return Err("missing skills".to_string());
    }

    Ok(manifest)
}

fn yaml_key_value(line: &str) -> Option<(&str, Option<String>)> {
    let (key, value) = line.split_once(':')?;
    Some((key.trim(), yaml_scalar(value.trim())))
}

fn yaml_scalar(value: &str) -> Option<String> {
    if value.is_empty() || value == "null" {
        return None;
    }
    Some(value.trim_matches('"').trim_matches('\'').to_string())
}

fn assign_skill_source_field(source: &mut SkillSourceMetadata, key: &str, value: Option<String>) {
    match key {
        "kind" => source.kind = value,
        "repo" => source.repo = value,
        "path" => source.path = value,
        "pinned_ref" => source.pinned_ref = value,
        "pinned_sha" => source.pinned_sha = value,
        "update_policy" => source.update_policy = value,
        "local_overlay" => source.local_overlay = value,
        _ => {}
    }
}

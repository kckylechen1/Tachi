//! Pure report builder for `tachi injection-surface doctor`.
//!
//! Credential plane checks use metadata only (path, mode, size) and never open
//! file contents. Other planes may read JSON structure; secret VALUES are never
//! copied into findings.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{
    DoctorSummary, Finding, InjectionSurfaceReport, PlaneAccount, PLANE_NAMES, SCHEMA_VERSION,
};

#[derive(Debug, Deserialize)]
struct FleetRegistryFile {
    #[serde(default)]
    harnesses: Vec<HarnessEntry>,
    #[serde(default)]
    retired_path_prefixes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HarnessEntry {
    harness_id: String,
    #[serde(default)]
    audience: Option<String>,
    #[serde(default)]
    expected_tachi_profile: Option<String>,
    #[serde(default)]
    retired_path_prefixes: Vec<String>,
    #[serde(default)]
    planes: BTreeMap<String, PlaneSpec>,
}

#[derive(Debug, Deserialize, Clone)]
struct PlaneSpec {
    #[serde(default)]
    path: Option<String>,
    #[serde(default = "default_scanned_true")]
    scanned: bool,
    #[serde(default)]
    budget_bytes: Option<u64>,
}

fn default_scanned_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct McpPlaneFile {
    #[serde(default)]
    registrations: Vec<McpRegistration>,
}

#[derive(Debug, Deserialize)]
struct McpRegistration {
    name: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PluginPlaneFile {
    #[serde(default)]
    entries: Vec<PluginEntry>,
    #[serde(default)]
    skill_roster: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PluginEntry {
    name: String,
    #[serde(default)]
    installed: bool,
    #[serde(default = "default_enabled_true")]
    enabled: bool,
    #[serde(default)]
    cache_dir: Option<String>,
}

fn default_enabled_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
struct EnvironmentPlaneFile {
    #[serde(default)]
    tachi_profile: Option<String>,
    #[serde(default)]
    injected_contracts: Vec<InjectedContract>,
    #[serde(default)]
    injected_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct InjectedContract {
    #[serde(default)]
    path: Option<String>,
}

struct LoadedHarness {
    harness_id: String,
    #[allow(dead_code)]
    audience: Option<String>,
    expected_tachi_profile: Option<String>,
    retired_path_prefixes: Vec<String>,
    planes: BTreeMap<String, PlaneSpec>,
}

struct PluginScan {
    harness_id: String,
    plane_path: PathBuf,
    cache_dirs: Vec<PathBuf>,
    skill_roster: Vec<PathBuf>,
    corpse_findings: Vec<Finding>,
}

/// Build a report-only injection-surface doctor report.
///
/// `home`, when set, is the root for resolving relative plane paths. When
/// unset, relative paths resolve against the registry file's parent directory.
pub(crate) fn build_report(
    registry_path: &Path,
    home: Option<&Path>,
) -> Result<InjectionSurfaceReport, String> {
    let registry_text = fs::read_to_string(registry_path)
        .map_err(|e| format!("read registry {}: {e}", registry_path.display()))?;
    let harnesses = parse_registry(&registry_text)?;
    let resolve_root = home
        .map(Path::to_path_buf)
        .or_else(|| registry_path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));

    let mut plane_accounts = Vec::new();
    let mut findings = Vec::new();
    let mut plugin_scans = Vec::new();
    let mut planes_scanned = 0usize;
    let mut planes_unscanned = 0usize;

    for harness in &harnesses {
        for plane_name in PLANE_NAMES {
            let spec = harness.planes.get(*plane_name);
            let scanned = spec.map(|s| s.scanned).unwrap_or(false);
            let resolved = spec
                .and_then(|s| s.path.as_ref())
                .map(|p| resolve_path(&resolve_root, p));

            if !scanned {
                planes_unscanned += 1;
                plane_accounts.push(PlaneAccount {
                    harness_id: harness.harness_id.clone(),
                    plane: (*plane_name).to_string(),
                    status: "unscanned".to_string(),
                    path: resolved.as_ref().map(|p| p.display().to_string()),
                });
                continue;
            }

            planes_scanned += 1;
            plane_accounts.push(PlaneAccount {
                harness_id: harness.harness_id.clone(),
                plane: (*plane_name).to_string(),
                status: "scanned".to_string(),
                path: resolved.as_ref().map(|p| p.display().to_string()),
            });

            let Some(path) = resolved else {
                findings.push(Finding {
                    harness_id: harness.harness_id.clone(),
                    plane: (*plane_name).to_string(),
                    check_kind: "missing_plane_path".to_string(),
                    evidence_path: format!("registry:{}", harness.harness_id),
                    severity: "CONCERN".to_string(),
                    remediation_owner: "fleet-registry".to_string(),
                });
                continue;
            };

            match *plane_name {
                "mcp" => findings.extend(scan_mcp(&harness.harness_id, &path)?),
                "plugin" => {
                    let scan = scan_plugin(&harness.harness_id, &path, &resolve_root)?;
                    findings.extend(scan.corpse_findings.clone());
                    plugin_scans.push(scan);
                }
                "credential" => findings.extend(scan_credential(&harness.harness_id, &path)?),
                "environment" => findings.extend(scan_environment(
                    harness,
                    &path,
                    &harness.retired_path_prefixes,
                )?),
                "density" => {
                    // First slice: density is accounted when scanned; budget
                    // overage is reserved for a later slice.
                    let _ = spec.and_then(|s| s.budget_bytes);
                    let _ = path;
                }
                _ => {}
            }
        }
    }

    findings.extend(scan_skill_sweep_ins(&plugin_scans));

    findings.sort_by(|a, b| {
        (
            a.harness_id.as_str(),
            a.plane.as_str(),
            a.check_kind.as_str(),
            a.evidence_path.as_str(),
        )
            .cmp(&(
                b.harness_id.as_str(),
                b.plane.as_str(),
                b.check_kind.as_str(),
                b.evidence_path.as_str(),
            ))
    });

    Ok(InjectionSurfaceReport {
        schema_version: SCHEMA_VERSION.to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        registry_path: registry_path.display().to_string(),
        home: home.map(|h| h.display().to_string()),
        summary: DoctorSummary {
            harnesses: harnesses.len(),
            planes_scanned,
            planes_unscanned,
            findings: findings.len(),
        },
        plane_accounts,
        findings,
    })
}

fn parse_registry(text: &str) -> Result<Vec<LoadedHarness>, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("parse fleet registry JSON: {e}"))?;

    let (entries, global_retired) = match value {
        Value::Array(arr) => {
            let entries: Vec<HarnessEntry> = serde_json::from_value(Value::Array(arr))
                .map_err(|e| format!("parse fleet registry array: {e}"))?;
            (entries, Vec::new())
        }
        Value::Object(_) => {
            let file: FleetRegistryFile = serde_json::from_value(value)
                .map_err(|e| format!("parse fleet registry object: {e}"))?;
            (file.harnesses, file.retired_path_prefixes)
        }
        other => {
            return Err(format!(
                "fleet registry must be a JSON array or object, got {}",
                value_kind(&other)
            ));
        }
    };

    Ok(entries
        .into_iter()
        .map(|entry| {
            let mut retired = entry.retired_path_prefixes;
            for prefix in &global_retired {
                if !retired.iter().any(|p| p == prefix) {
                    retired.push(prefix.clone());
                }
            }
            LoadedHarness {
                harness_id: entry.harness_id,
                audience: entry.audience,
                expected_tachi_profile: entry.expected_tachi_profile,
                retired_path_prefixes: retired,
                planes: entry.planes,
            }
        })
        .collect())
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn resolve_path(root: &Path, raw: &str) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn scan_mcp(harness_id: &str, path: &Path) -> Result<Vec<Finding>, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("read mcp plane {}: {e}", path.display()))?;
    let file: McpPlaneFile =
        serde_json::from_str(&text).map_err(|e| format!("parse mcp plane {}: {e}", path.display()))?;

    let mut by_name: BTreeMap<String, Vec<&McpRegistration>> = BTreeMap::new();
    for reg in &file.registrations {
        by_name.entry(reg.name.clone()).or_default().push(reg);
    }

    let mut findings = Vec::new();
    for (name, regs) in by_name {
        if regs.len() < 2 {
            continue;
        }
        let scopes: BTreeSet<_> = regs.iter().filter_map(|r| r.scope.as_deref()).collect();
        let endpoints: BTreeSet<_> = regs.iter().filter_map(|r| r.endpoint.as_deref()).collect();
        let split = scopes.len() > 1 || endpoints.len() > 1;
        if !split {
            continue;
        }
        findings.push(Finding {
            harness_id: harness_id.to_string(),
            plane: "mcp".to_string(),
            check_kind: "mcp_identity_split".to_string(),
            evidence_path: format!("{}#server={name}", path.display()),
            severity: "BUG".to_string(),
            remediation_owner: "harness-mcp".to_string(),
        });
    }
    Ok(findings)
}

fn scan_plugin(
    harness_id: &str,
    path: &Path,
    resolve_root: &Path,
) -> Result<PluginScan, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("read plugin plane {}: {e}", path.display()))?;
    let file: PluginPlaneFile = serde_json::from_str(&text)
        .map_err(|e| format!("parse plugin plane {}: {e}", path.display()))?;

    let mut cache_dirs = Vec::new();
    let mut corpse_findings = Vec::new();

    for entry in &file.entries {
        if let Some(cache) = &entry.cache_dir {
            let cache_path = resolve_path(resolve_root, cache);
            if cache_path.is_dir() {
                cache_dirs.push(cache_path.clone());
            }
            // Corpse: installed-but-disabled + cache dir present on disk.
            if entry.installed && !entry.enabled && cache_path.is_dir() {
                corpse_findings.push(Finding {
                    harness_id: harness_id.to_string(),
                    plane: "plugin".to_string(),
                    check_kind: "plugin_corpse".to_string(),
                    evidence_path: format!(
                        "{}#plugin={}",
                        cache_path.display(),
                        entry.name
                    ),
                    severity: "CONCERN".to_string(),
                    remediation_owner: "harness-plugin".to_string(),
                });
            }
        }
    }

    let skill_roster = file
        .skill_roster
        .iter()
        .map(|p| resolve_path(resolve_root, p))
        .collect();

    Ok(PluginScan {
        harness_id: harness_id.to_string(),
        plane_path: path.to_path_buf(),
        cache_dirs,
        skill_roster,
        corpse_findings,
    })
}

fn scan_skill_sweep_ins(scans: &[PluginScan]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (idx, scan) in scans.iter().enumerate() {
        for (other_idx, other) in scans.iter().enumerate() {
            if idx == other_idx {
                continue;
            }
            for roster_path in &scan.skill_roster {
                for foreign_cache in &other.cache_dirs {
                    if path_under(roster_path, foreign_cache) {
                        findings.push(Finding {
                            harness_id: scan.harness_id.clone(),
                            plane: "plugin".to_string(),
                            check_kind: "skill_sweep_in".to_string(),
                            evidence_path: format!(
                                "{} (under {} cache {})",
                                roster_path.display(),
                                other.harness_id,
                                foreign_cache.display()
                            ),
                            severity: "BUG".to_string(),
                            remediation_owner: "harness-plugin".to_string(),
                        });
                    }
                }
            }
        }
    }
    let _ = scans.first().map(|s| &s.plane_path);
    findings
}

fn path_under(path: &Path, ancestor: &Path) -> bool {
    let Ok(path) = path.canonicalize() else {
        // Fall back to prefix compare on the unresolved forms for fixtures
        // that may not fully exist on disk.
        return path.starts_with(ancestor);
    };
    let Ok(ancestor) = ancestor.canonicalize() else {
        return false;
    };
    path.starts_with(&ancestor)
}

/// Credential plane: metadata only — never open file contents.
fn scan_credential(harness_id: &str, path: &Path) -> Result<Vec<Finding>, String> {
    let meta = fs::metadata(path)
        .map_err(|e| format!("stat credential plane {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Ok(Vec::new());
    }

    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !looks_credential_like(&file_name) {
        return Ok(Vec::new());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        // Group- or world-readable.
        if mode & 0o044 != 0 {
            return Ok(vec![Finding {
                harness_id: harness_id.to_string(),
                plane: "credential".to_string(),
                check_kind: "credential_world_readable".to_string(),
                evidence_path: format!("{} mode={mode:#o}", path.display()),
                severity: "BUG".to_string(),
                remediation_owner: "harness-credential".to_string(),
            }]);
        }
    }

    #[cfg(not(unix))]
    {
        let _ = harness_id;
    }

    Ok(Vec::new())
}

fn looks_credential_like(file_name: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "credential",
        "credentials",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "password",
        ".env",
        "auth",
        "jwt",
    ];
    NEEDLES.iter().any(|n| file_name.contains(n))
}

fn scan_environment(
    harness: &LoadedHarness,
    path: &Path,
    retired_prefixes: &[String],
) -> Result<Vec<Finding>, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("read environment plane {}: {e}", path.display()))?;
    let file: EnvironmentPlaneFile = serde_json::from_str(&text)
        .map_err(|e| format!("parse environment plane {}: {e}", path.display()))?;

    let mut findings = Vec::new();

    if let (Some(expected), Some(actual)) = (
        harness.expected_tachi_profile.as_deref(),
        file.tachi_profile.as_deref(),
    ) {
        if expected != actual {
            findings.push(Finding {
                harness_id: harness.harness_id.clone(),
                plane: "environment".to_string(),
                check_kind: "tachi_profile_mismatch".to_string(),
                evidence_path: format!(
                    "{} expected={expected} actual={actual}",
                    path.display()
                ),
                severity: "BUG".to_string(),
                remediation_owner: "harness-tachi-client".to_string(),
            });
        }
    }

    let mut paths: Vec<String> = file.injected_paths.clone();
    for contract in &file.injected_contracts {
        if let Some(p) = &contract.path {
            paths.push(p.clone());
        }
    }

    for injected in paths {
        for prefix in retired_prefixes {
            if path_matches_retired_prefix(&injected, prefix) {
                findings.push(Finding {
                    harness_id: harness.harness_id.clone(),
                    plane: "environment".to_string(),
                    check_kind: "env_ghost".to_string(),
                    evidence_path: format!("{}#path={injected}", path.display()),
                    severity: "CONCERN".to_string(),
                    remediation_owner: "harness-environment".to_string(),
                });
                break;
            }
        }
    }

    Ok(findings)
}

fn path_matches_retired_prefix(path: &str, prefix: &str) -> bool {
    let normalized = path.trim_start_matches("~/").trim_start_matches('/');
    let prefix = prefix.trim_start_matches("~/").trim_start_matches('/');
    normalized.starts_with(prefix) || path.contains(prefix)
}

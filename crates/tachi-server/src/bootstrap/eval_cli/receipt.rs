//! Private replay receipts and conservative, paired regression comparison.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::memory_search_ops::recall_simulate::RecallSimulationParams;

const SCHEMA: &str = "tachi.recall_eval.run.v1";

pub(super) fn validate_output_path(path: &Path, app_home: &Path) -> Result<(), std::io::Error> {
    // Resolve existing ancestors as well as an existing final path, so a new
    // output through a parent symlink cannot alias the status writer's paths.
    fn resolve(path: &Path) -> Result<PathBuf, std::io::Error> {
        match std::fs::canonicalize(path) {
            Ok(path) => return Ok(path),
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("invalid receipt parent"))?;
        let name = path
            .file_name()
            .ok_or_else(|| std::io::Error::other("invalid receipt filename"))?;
        Ok(resolve(parent)?.join(name))
    }
    let absolute = |path: &Path| -> Result<PathBuf, std::io::Error> {
        Ok(if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        })
    };
    let output = resolve(&absolute(path)?)?;
    let status = crate::status_ops::recall_eval::recall_eval_status_path(app_home);
    for reserved in [&status, &status.with_extension("json.tmp")] {
        // New components retain their spelling even on case-insensitive
        // filesystems. Conservatively reserve their ASCII case variants too.
        if output
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&resolve(&absolute(reserved)?)?.as_os_str().to_string_lossy())
        {
            return Err(std::io::Error::other(
                "receipt output aliases a reserved shared-status path",
            ));
        }
    }
    Ok(())
}

pub(super) fn write_receipt(
    path: &Path,
    compare_path: Option<&Path>,
    params: &RecallSimulationParams,
    report: &Value,
    status: &mut Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let inputs = json!({
        "cases": params.metadata.as_ref().and_then(|value| value.get("cases")),
        "top_k": params.top_k,
        "enable_rerank": params.enable_rerank,
        "label_policy": status["gate_label_policy"],
        "loaded_label_counts": status["loaded_label_counts"],
        "skipped_rows": status["skipped_rows"],
        "disabled_rows": status["disabled_rows"],
        "thresholds": status["thresholds"],
    });
    if report["receipt_context"]["stable"].as_bool() != Some(true) {
        status["status"] = json!("incomplete");
        status["incomplete_reason"] = json!("receipt inputs changed or could not be established");
    }
    let mut receipt = json!({
        "schema_version": SCHEMA,
        "run_id": uuid::Uuid::new_v4().to_string(),
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "build": {
            "git_sha": crate::build_info::GIT_SHA,
            "build_time": crate::build_info::BUILD_TIME,
            "version": crate::build_info::PKG_VERSION,
            "binary_sha256": file_digest(&std::env::current_exe()?)?,
        },
        "inputs_sha256": digest(&inputs)?,
        "inputs": inputs,
        "replay": report,
        "status": status.clone(),
        "limits": [
            "Store generations identify the same physical database copy, not portable content hashes.",
            "Wall-clock decay is not frozen by as_of; timings are descriptive single-run observations.",
            "Automatic paired gates require query embeddings disabled and rerank disabled; online model runs remain diagnostic.",
            "Replay measures retrieval evidence, not answer synthesis, citation correctness or a full host context budget."
        ],
    });
    if let Some(baseline_path) = compare_path {
        if std::fs::metadata(baseline_path)?.len() > 64 * 1024 * 1024 {
            return Err("baseline receipt exceeds 64 MiB".into());
        }
        let baseline: Value = serde_json::from_slice(&std::fs::read(baseline_path)?)?;
        let comparison = compare(&baseline, &receipt)?;
        match comparison["status"].as_str() {
            Some("incompatible") => {
                status["status"] = json!("incomplete");
                status["incomplete_reason"] = json!("paired receipt comparison is incompatible");
            }
            Some("regressed") => status["status"] = json!("failed"),
            _ => {}
        }
        receipt["comparison"] = comparison;
        receipt["status"] = status.clone();
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    // Reuse private, synced staging. A same-directory hard link publishes the
    // complete file atomically and refuses an existing destination on every
    // supported filesystem; never fall back to a clobbering rename.
    let staging = parent.join(format!(".recall-eval-{}.tmp", uuid::Uuid::new_v4()));
    crate::utils::write_owner_only_file_atomic(&staging, &serde_json::to_vec_pretty(&receipt)?)?;
    let published = std::fs::hard_link(&staging, path);
    let _ = std::fs::remove_file(&staging);
    published?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn digest(value: &Value) -> Result<String, serde_json::Error> {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
            Value::Object(values) => {
                let sorted: std::collections::BTreeMap<_, _> = values.iter().collect();
                Value::Object(
                    sorted
                        .into_iter()
                        .map(|(key, value)| (key.clone(), canonical(value)))
                        .collect(),
                )
            }
            _ => value.clone(),
        }
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical(value))?)
    ))
}

fn file_digest(path: &Path) -> Result<String, std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        hash.update(&bytes[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn compare(baseline: &Value, candidate: &Value) -> Result<Value, serde_json::Error> {
    let incompatible = |reason: &str| json!({"status": "incompatible", "reason": reason});
    for receipt in [baseline, candidate] {
        if receipt["schema_version"] != SCHEMA
            || !receipt["inputs"].is_object()
            || receipt["inputs_sha256"] != digest(&receipt["inputs"])?
            || receipt["replay"]["receipt_context"]["stable"] != true
            || !receipt["replay"]["receipt_context"]["before"].is_object()
            || receipt["replay"]["receipt_context"]["before"]
                != receipt["replay"]["receipt_context"]["after"]
            || !matches!(receipt["status"]["status"].as_str(), Some("ok" | "failed"))
        {
            return Ok(incompatible(
                "receipt is incomplete, malformed, or mutated during replay",
            ));
        }
    }
    if baseline["inputs_sha256"] != candidate["inputs_sha256"] {
        return Ok(incompatible(
            "corpus, labels, cutoffs, or thresholds differ",
        ));
    }
    let before = &baseline["replay"]["receipt_context"]["before"];
    let after = &candidate["replay"]["receipt_context"]["before"];
    for context in [before, after] {
        if context["embedding"]["disabled"] != true || context["rerank"]["enabled"] != false {
            return Ok(incompatible(
                "automatic paired gates require offline lexical replay",
            ));
        }
    }
    // RecallConfig and code are the treatment. Everything else is a control.
    let mut control_before = before.clone();
    let mut control_after = after.clone();
    for context in [&mut control_before, &mut control_after] {
        if let Some(object) = context.as_object_mut() {
            object.remove("recall_config");
        }
    }
    if control_before != control_after {
        return Ok(incompatible(
            "store generations, routing, provider settings or effective requests differ",
        ));
    }
    let Some(old_cases) = baseline["replay"]["cases"].as_array() else {
        return Ok(incompatible("baseline has no cases"));
    };
    let Some(new_cases) = candidate["replay"]["cases"].as_array() else {
        return Ok(incompatible("candidate has no cases"));
    };
    if old_cases.is_empty()
        || old_cases.len() != new_cases.len()
        || Some(old_cases.len()) != candidate["inputs"]["cases"].as_array().map(Vec::len)
    {
        return Ok(incompatible("case alignment is incomplete"));
    }
    let mut differences = Vec::new();
    let mut regressions = 0;
    let mut hit_to_miss = 0;
    let mut miss_to_hit = 0;
    let mut improved = 0;
    for (idx, (old, new)) in old_cases.iter().zip(new_cases).enumerate() {
        if !old["error"].is_null()
            || !new["error"].is_null()
            || !old["expectation_passed"].is_boolean()
            || !new["expectation_passed"].is_boolean()
            || !old["returned_ids"].is_array()
            || !new["returned_ids"].is_array()
            || old["query"] != new["query"]
            || old["expected_ids"] != new["expected_ids"]
        {
            return Ok(incompatible(
                "case errors or invalid aligned results cannot be scored as flips",
            ));
        }
        for case in [old, new] {
            let returned = case["returned_ids"].as_array().unwrap();
            let Some(expected) = case["expected_ids"].as_array() else {
                return Ok(incompatible("expected IDs missing"));
            };
            let measured = returned
                .iter()
                .position(|id| expected.contains(id))
                .map(|idx| idx as u64 + 1);
            if case["rank"] != json!(measured) {
                return Ok(incompatible("stored rank disagrees with returned evidence"));
            }
        }
        let old_rank = old["rank"].as_u64();
        let new_rank = new["rank"].as_u64();
        let lost = old_rank.is_some() && new_rank.is_none();
        let gained = old_rank.is_none() && new_rank.is_some();
        let rank_worse = matches!((old_rank, new_rank), (Some(old), Some(new)) if new > old);
        let contract_worse =
            old["expectation_passed"] == true && new["expectation_passed"] == false;
        let regressed = lost || rank_worse || contract_worse;
        regressions += usize::from(regressed);
        hit_to_miss += usize::from(lost);
        miss_to_hit += usize::from(gained);
        improved +=
            usize::from(matches!((old_rank, new_rank), (Some(old), Some(new)) if new < old));
        differences.push(json!({
            "case_index": idx, "name": new["name"],
            "before_rank": old_rank, "after_rank": new_rank,
            "before_passed": old["expectation_passed"], "after_passed": new["expectation_passed"],
            "returned_order_changed": old["returned_ids"] != new["returned_ids"],
            "regressed": regressed,
            "before_elapsed_ms": old["replay_elapsed_ms"], "after_elapsed_ms": new["replay_elapsed_ms"],
        }));
    }
    Ok(json!({
        "status": if regressions > 0 { "regressed" } else { "no_regression_observed" },
        "baseline_run_id": baseline["run_id"], "candidate_run_id": candidate["run_id"],
        "compared_cases": differences.len(), "regressions": regressions,
        "hit_to_miss": hit_to_miss, "miss_to_hit": miss_to_hit, "rank_improved": improved,
        "recall_config_changed": before["recall_config"] != after["recall_config"],
        "cases": differences,
        "interpretation": "paired observations, not a statistical quality or latency claim",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(rank: Option<u64>) -> Value {
        let inputs = json!({"cases": [{"query": "q", "expected_id": "id"}]});
        let context = json!({"embedding": {"disabled": true}, "rerank": {"enabled": false},
            "store_generations": [{"identity": "fixture", "generation": 1}], "recall_config": {"rrf_k": 20}});
        let returned = rank
            .map(|rank| {
                (1..=rank)
                    .map(|idx| {
                        if idx == rank {
                            "id".to_string()
                        } else {
                            format!("other-{idx}")
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        json!({"schema_version": SCHEMA, "inputs_sha256": digest(&inputs).unwrap(), "inputs": inputs,
            "status": {"status": "ok"}, "replay": {"receipt_context": {"stable": true, "before": context, "after": context},
            "cases": [{"query": "q", "expected_ids": ["id"], "rank": rank,
                "expectation_passed": rank.is_some(), "returned_ids": returned, "error": null}]}})
    }

    #[test]
    fn paired_receipts_distinguish_regression_from_incompatible_or_error_runs() {
        let baseline = receipt(Some(1));
        assert_eq!(
            compare(&baseline, &receipt(Some(3))).unwrap()["status"],
            "regressed"
        );
        assert_eq!(
            compare(&baseline, &receipt(None)).unwrap()["hit_to_miss"],
            1
        );
        let mut candidate = receipt(Some(1));
        candidate["replay"]["receipt_context"]["before"]["store_generations"][0]["generation"] =
            json!(2);
        candidate["replay"]["receipt_context"]["after"]["store_generations"][0]["generation"] =
            json!(2);
        assert_eq!(
            compare(&baseline, &candidate).unwrap()["status"],
            "incompatible"
        );
        let mut error = receipt(None);
        error["replay"]["cases"][0]["error"] = json!("failed");
        assert_eq!(
            compare(&error, &baseline).unwrap()["status"],
            "incompatible"
        );
        let mut drift = receipt(Some(1));
        drift["inputs"]["cases"][0]["expected_id"] = json!("different");
        assert_eq!(
            compare(&baseline, &drift).unwrap()["status"],
            "incompatible"
        );
    }

    #[test]
    fn receipt_destination_cannot_alias_either_status_writer_path() {
        let dir = tempfile::tempdir().unwrap();
        let status = crate::status_ops::recall_eval::recall_eval_status_path(dir.path());
        assert!(!status.exists());
        assert!(validate_output_path(&status, dir.path()).is_err());
        assert!(validate_output_path(&status.with_extension("json.tmp"), dir.path()).is_err());
        for name in [
            "STATUS/Recall_Eval.Latest.JSON",
            "STATUS/Recall_Eval.Latest.JSON.TMP",
        ] {
            let alias = dir.path().join(name);
            assert!(!alias.exists());
            assert!(validate_output_path(&alias, dir.path()).is_err());
        }
        assert!(validate_output_path(&dir.path().join("runs/new.json"), dir.path()).is_ok());
        #[cfg(unix)]
        {
            let alias = dir.path().join("home-alias");
            std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
            assert!(validate_output_path(
                &alias.join("status/recall_eval.latest.json"),
                dir.path()
            )
            .is_err());
        }
    }
}

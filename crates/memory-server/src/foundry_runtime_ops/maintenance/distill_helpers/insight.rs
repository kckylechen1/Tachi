use crate::foundry_runtime_ops::helpers::round3;
use crate::foundry_runtime_ops::FOUNDRY_RELATED_LIMIT;
use memory_core::MemoryEntry;
use serde_json::json;

pub(in crate::foundry_runtime_ops) fn infer_memory_insight(
    entry: &MemoryEntry,
    avg_importance: f64,
    contradiction_count: u32,
    same_topic_count: u32,
    related_count: usize,
) -> serde_json::Value {
    let surprise =
        memory_core::surprise_score(entry, avg_importance, contradiction_count, same_topic_count);
    let mut reasons = Vec::new();

    if contradiction_count > 0 {
        reasons.push("contradiction".to_string());
    }
    if same_topic_count <= 1 {
        reasons.push("novel_topic".to_string());
    }
    if entry.access_count == 0 && entry.importance > 0.7 {
        reasons.push("overlooked_high_importance".to_string());
    }
    if (entry.importance - avg_importance).abs() >= 0.25 {
        reasons.push("importance_outlier".to_string());
    }
    if related_count >= FOUNDRY_RELATED_LIMIT {
        reasons.push("dense_neighborhood".to_string());
    }

    json!({
        "kind": "memory_insight",
        "surprise": round3(surprise),
        "priority": if surprise >= 0.4 { "high" } else if surprise >= 0.2 { "medium" } else { "low" },
        "reasons": reasons,
        "signals": {
            "avg_importance": round3(avg_importance),
            "importance_delta": round3(entry.importance - avg_importance),
            "contradiction_count": contradiction_count,
            "same_topic_count": same_topic_count,
            "related_count": related_count,
        }
    })
}

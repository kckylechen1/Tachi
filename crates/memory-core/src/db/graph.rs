use rusqlite::{params, Connection};
use std::collections::HashSet;

use crate::error::MemoryError;
use crate::types::{GraphExpandResult, MemoryEdge};

use super::common::{normalize_utc_iso_or_now, now_utc_iso};
use super::memory_crud::fetch_by_ids;

pub fn add_edge(conn: &Connection, edge: &MemoryEdge) -> Result<(), MemoryError> {
    let created = if edge.created_at.is_empty() {
        now_utc_iso()
    } else {
        normalize_utc_iso_or_now(&edge.created_at)
    };
    let valid_from = if edge.valid_from.is_empty() {
        created.clone()
    } else {
        normalize_utc_iso_or_now(&edge.valid_from)
    };
    let meta_str = serde_json::to_string(&edge.metadata).unwrap_or_else(|_| "{}".to_string());

    conn.execute(
        r#"INSERT INTO memory_edges (source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
           ON CONFLICT(source_id, target_id, relation)
           DO UPDATE SET weight = ?4, metadata = ?5, created_at = ?6, valid_from = ?7, valid_to = ?8"#,
        params![edge.source_id, edge.target_id, edge.relation, edge.weight, meta_str, created, valid_from, edge.valid_to],
    )?;
    Ok(())
}

/// Remove an edge from the memory graph.
pub fn remove_edge(
    conn: &Connection,
    source_id: &str,
    target_id: &str,
    relation: &str,
) -> Result<bool, MemoryError> {
    let count = conn.execute(
        "DELETE FROM memory_edges WHERE source_id = ?1 AND target_id = ?2 AND relation = ?3",
        params![source_id, target_id, relation],
    )?;
    Ok(count > 0)
}

/// Get all edges connected to a memory ID.
/// direction: "outgoing" (source_id match), "incoming" (target_id match), or "both"
pub fn get_edges(
    conn: &Connection,
    memory_id: &str,
    direction: &str,
    relation_filter: Option<&str>,
) -> Result<Vec<MemoryEdge>, MemoryError> {
    let base_sql = match direction {
        "incoming" =>
            "SELECT source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to FROM memory_edges WHERE target_id = ?1
             AND (valid_to IS NULL OR valid_to > datetime('now'))",
        "outgoing" =>
            "SELECT source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to FROM memory_edges WHERE source_id = ?1
             AND (valid_to IS NULL OR valid_to > datetime('now'))",
        _ =>
            "SELECT source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to FROM memory_edges WHERE (source_id = ?1 OR target_id = ?1)
             AND (valid_to IS NULL OR valid_to > datetime('now'))",
    };

    // Use parameterized query for relation_filter to prevent SQL injection
    let full_sql = if relation_filter.is_some() {
        format!("{} AND relation = ?2", base_sql)
    } else {
        base_sql.to_string()
    };

    let mut stmt = conn.prepare(&full_sql)?;
    let row_mapper = |row: &rusqlite::Row| {
        let meta_str: String = row.get(4)?;
        let metadata = serde_json::from_str(&meta_str).unwrap_or_default();
        Ok(MemoryEdge {
            source_id: row.get(0)?,
            target_id: row.get(1)?,
            relation: row.get(2)?,
            weight: row.get(3)?,
            metadata,
            created_at: row.get(5)?,
            valid_from: row.get(6)?,
            valid_to: row.get(7)?,
        })
    };

    let mut edges = Vec::new();
    if let Some(rel) = relation_filter {
        for row in stmt.query_map(params![memory_id, rel], row_mapper)? {
            edges.push(row?);
        }
    } else {
        for row in stmt.query_map(params![memory_id], row_mapper)? {
            edges.push(row?);
        }
    }

    Ok(edges)
}

/// Count the number of 'contradicts' edges for a given memory ID.
/// Used by surprise scoring to detect controversial/surprising memories.
pub fn get_contradiction_count(conn: &Connection, memory_id: &str) -> Result<u32, MemoryError> {
    let count: u32 = conn.query_row(
        "SELECT COUNT(*) FROM memory_edges WHERE (source_id = ?1 OR target_id = ?1) AND relation = 'contradicts' AND (valid_to IS NULL OR valid_to > datetime('now'))",
        params![memory_id],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Count how many memories share the same topic as the given entry.
/// Used by surprise scoring for topic novelty.
pub fn count_same_topic(conn: &Connection, topic: &str) -> Result<u32, MemoryError> {
    if topic.is_empty() {
        return Ok(0);
    }
    let count: u32 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE topic = ?1 AND archived = 0",
        params![topic],
        |row| row.get(0),
    )?;
    Ok(count)
}

/// Get the average importance across all non-archived memories.
/// Used by surprise scoring.
pub fn avg_importance(conn: &Connection) -> Result<f64, MemoryError> {
    let avg: f64 = conn.query_row(
        "SELECT COALESCE(AVG(importance), 0.7) FROM memories WHERE archived = 0",
        [],
        |row| row.get(0),
    )?;
    Ok(avg)
}

/// Return IDs whose memory row has been superseded by a newer memory.
pub fn get_superseded_ids(
    conn: &Connection,
    ids: &[String],
) -> Result<HashSet<String>, MemoryError> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }

    let mut out = HashSet::new();
    for batch in ids.chunks(500) {
        let placeholders = (1..=batch.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id FROM memories WHERE id IN ({}) AND superseded_by IS NOT NULL",
            placeholders
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(batch.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            out.insert(row?);
        }
    }
    Ok(out)
}

/// BFS graph expansion from seed memory IDs.
/// Returns entries found within `max_hops` of any seed, plus the edges traversed.
pub fn graph_expand(
    conn: &Connection,
    seed_ids: &[String],
    max_hops: u32,
    relation_filter: Option<&str>,
) -> Result<GraphExpandResult, MemoryError> {
    use std::collections::{HashMap, HashSet, VecDeque};

    let mut visited: HashSet<String> = HashSet::new();
    let mut distances: HashMap<String, u32> = HashMap::new();
    let mut all_edges: Vec<MemoryEdge> = Vec::new();
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();

    // Initialize with seeds at distance 0
    for id in seed_ids {
        if visited.insert(id.clone()) {
            distances.insert(id.clone(), 0);
            queue.push_back((id.clone(), 0));
        }
    }

    // BFS with max_nodes throttle (protect context window)
    const MAX_NODES: usize = 50;
    while let Some((current_id, depth)) = queue.pop_front() {
        if depth >= max_hops {
            continue;
        }
        if visited.len() >= MAX_NODES {
            break;
        }

        let edges = get_edges(conn, &current_id, "both", relation_filter)?;
        for edge in &edges {
            let neighbor = if edge.source_id == current_id {
                &edge.target_id
            } else {
                &edge.source_id
            };

            if visited.insert(neighbor.clone()) {
                let new_depth = depth + 1;
                distances.insert(neighbor.clone(), new_depth);
                queue.push_back((neighbor.clone(), new_depth));
            }
        }
        all_edges.extend(edges);
    }

    // Fetch all discovered entries (exclude seeds — caller already has those)
    let non_seed_ids: Vec<String> = distances
        .keys()
        .filter(|id| !seed_ids.contains(id))
        .cloned()
        .collect();

    let entries = if non_seed_ids.is_empty() {
        Vec::new()
    } else {
        let map = fetch_by_ids(conn, &non_seed_ids, false)?;
        map.into_values().collect()
    };

    // Deduplicate edges
    all_edges.sort_by(|a, b| {
        (&a.source_id, &a.target_id, &a.relation).cmp(&(&b.source_id, &b.target_id, &b.relation))
    });
    all_edges.dedup_by(|a, b| {
        a.source_id == b.source_id && a.target_id == b.target_id && a.relation == b.relation
    });

    Ok(GraphExpandResult {
        entries,
        edges: all_edges,
        distances,
    })
}

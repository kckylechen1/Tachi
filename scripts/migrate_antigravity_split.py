#!/usr/bin/env python3
"""Phase 1.2 — split antigravity/memory.db into project-scoped DBs.

Architect-curated path → project mapping. Records that match a destination
prefix are MOVED (insert-or-replace into target DB, then deleted from source).
Records flagged as ``antigravity_keep`` stay where they are. Foundry hallucination
records were already purged in Phase 1.1.

Run:  python3 scripts/migrate_antigravity_split.py [--dry-run]
"""

from __future__ import annotations

import argparse
import json
import re
import sqlite3
import sys
from pathlib import Path

TACHI_ROOT = Path.home() / ".tachi"
SOURCE_DB = TACHI_ROOT / "projects" / "antigravity" / "memory.db"
GLOBAL_DB = TACHI_ROOT / "global" / "memory.db"
PROJECTS_DIR = TACHI_ROOT / "projects"


def table_columns(conn: sqlite3.Connection, table: str) -> set[str]:
    return {row[1] for row in conn.execute(f"PRAGMA table_info({table})")}


def json_list(raw) -> list[str]:
    try:
        value = json.loads(raw or "[]")
    except (TypeError, json.JSONDecodeError):
        return []
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, str) and item]


def entities_with_legacy_persons(rec: dict) -> str:
    entities = json_list(rec.get("entities"))
    seen = {entity.casefold() for entity in entities}
    for person in json_list(rec.get("persons")):
        key = person.casefold()
        if key not in seen:
            entities.append(person)
            seen.add(key)
    return json.dumps(entities, ensure_ascii=False)


def classify(path: str, text: str) -> str:
    """Return destination project key, or ``"keep"`` to leave in antigravity."""
    p = path or ""
    # Strong path-prefix matches (highest signal).
    if p.startswith("/hapi") or p == "/hapi":
        return "hapi"
    if p.startswith("/openclaw") or p.startswith("/project/openclaw"):
        return "openclaw"
    if p.startswith("/tachi"):
        return "tachi"
    if p.startswith("/project/sigil"):
        return "sigil"
    if (
        p.startswith("/project/quant")
        or p.startswith("/project/Quant_Analyzer")
        or p.startswith("/Quant_Analyzer")
        or p.startswith("/quant_analyzer")
        or re.match(r"^/project/(股票|交易|量化|投资|因子|选股|回测)", p)
    ):
        return "quant"
    if p.startswith("/hyperion") or p.startswith("/project/Hyperion"):
        return "hyperion"
    # Antigravity-native or routing artefacts stay.
    if (
        p.startswith("/antigravity")
        or p.startswith("/project/antigravity")
        or p.startswith("/kanban/antigravity")
        or p.startswith("/kanban/amp")
    ):
        return "keep"
    # /user/* → global (per Tachi scoping rules).
    if p.startswith("/user"):
        return "global"
    # Sensitive credentials → global vault scope.
    if p.startswith("/nexu/credentials") or "credentials" in p:
        return "global"

    # Soft heuristics on text content for ambiguous /project/* and root-level paths.
    t = (text or "").lower()
    hapi_markers = (
        "v8", "hapi", "evolution_guard", "watchlist", "portfoliomanager",
        "signal_report", "quant_core", "warpcore", "v8_score", "缠论",
        "持仓", "选股", "因子", "回测", "策略", "舰长", "engine/v8",
    )
    if any(m in t for m in hapi_markers) or any(m in (p or "").lower() for m in ("v8", "hapi")):
        return "hapi"
    sigil_markers = ("sigil", "memory-core", "memory-server", "tachi-mcp")
    if any(m in t for m in sigil_markers):
        return "sigil"
    openclaw_markers = ("openclaw", "open-claw")
    if any(m in t for m in openclaw_markers):
        return "openclaw"
    dragonfly_markers = ("dragonfly", "openalice")
    if any(m in t for m in dragonfly_markers):
        # No dragonfly DB exists — treat as antigravity-keep noise for now.
        return "keep"

    # Unknown → keep in antigravity (conservative default).
    return "keep"


def fetch_rows(conn: sqlite3.Connection):
    cols = table_columns(conn, "memories")
    persons_expr = "persons" if "persons" in cols else "'[]' AS persons"
    cur = conn.execute(
        "SELECT id, path, summary, text, importance, timestamp, category, topic, "
        f"keywords, {persons_expr}, entities, location, source, scope, archived, "
        "created_at, updated_at, access_count, last_access, revision, metadata, "
        "retention_policy, domain FROM memories"
    )
    cols = [d[0] for d in cur.description]
    for row in cur.fetchall():
        yield dict(zip(cols, row))


def fetch_edges_for(conn: sqlite3.Connection, ids: set[str]):
    if not ids:
        return []
    placeholders = ",".join("?" for _ in ids)
    sql = (
        f"SELECT source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to "
        f"FROM memory_edges WHERE source_id IN ({placeholders}) OR target_id IN ({placeholders})"
    )
    return list(conn.execute(sql, list(ids) + list(ids)).fetchall())


def ensure_project_db(project: str) -> Path:
    if project == "global":
        return GLOBAL_DB
    proj_dir = PROJECTS_DIR / project
    proj_dir.mkdir(parents=True, exist_ok=True)
    return proj_dir / "memory.db"


def insert_record(conn: sqlite3.Connection, rec: dict):
    conn.execute(
        """INSERT OR REPLACE INTO memories (
            id, path, summary, text, importance, timestamp, category, topic,
            keywords, entities, location, source, scope, archived,
            created_at, updated_at, access_count, last_access, revision, metadata,
            retention_policy, domain
        ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
        (
            rec["id"], rec["path"], rec["summary"], rec["text"], rec["importance"],
            rec["timestamp"], rec["category"], rec["topic"], rec["keywords"],
            entities_with_legacy_persons(rec), rec["location"], rec["source"],
            rec["scope"], rec["archived"], rec["created_at"], rec["updated_at"],
            rec["access_count"], rec["last_access"], rec["revision"], rec["metadata"],
            rec["retention_policy"], rec["domain"],
        ),
    )


def insert_edge(conn: sqlite3.Connection, edge: tuple):
    conn.execute(
        """INSERT OR IGNORE INTO memory_edges (
            source_id, target_id, relation, weight, metadata, created_at, valid_from, valid_to
        ) VALUES (?,?,?,?,?,?,?,?)""",
        edge,
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    if not SOURCE_DB.exists():
        sys.exit(f"source DB missing: {SOURCE_DB}")

    src = sqlite3.connect(str(SOURCE_DB))
    src.row_factory = sqlite3.Row

    moves: dict[str, list[dict]] = {}
    keep = 0
    for rec in fetch_rows(src):
        dest = classify(rec["path"], rec["text"])
        if dest == "keep":
            keep += 1
            continue
        moves.setdefault(dest, []).append(rec)

    print("=== Migration plan ===")
    for k, v in sorted(moves.items(), key=lambda kv: -len(kv[1])):
        print(f"  → {k:10s} : {len(v)} records")
    print(f"  → keep      : {keep} records (stay in antigravity)")
    total = sum(len(v) for v in moves.values()) + keep
    print(f"  TOTAL       : {total}")

    if args.dry_run:
        print("dry-run: no writes")
        return

    moved_ids: set[str] = set()
    for project, recs in moves.items():
        dest_path = ensure_project_db(project)
        if not dest_path.exists():
            sys.exit(f"target DB missing (must be initialised by tachi-server first): {dest_path}")
        dst = sqlite3.connect(str(dest_path))
        try:
            dst.execute("BEGIN")
            for rec in recs:
                insert_record(dst, rec)
                moved_ids.add(rec["id"])
            ids_in_batch = {r["id"] for r in recs}
            for edge in fetch_edges_for(src, ids_in_batch):
                insert_edge(dst, edge)
            dst.commit()
            print(f"  ✓ wrote {len(recs)} → {dest_path}")
        except Exception as exc:
            dst.rollback()
            sys.exit(f"failed writing to {dest_path}: {exc}")
        finally:
            dst.close()

    # Delete migrated rows from source.
    src.execute("BEGIN")
    placeholders = ",".join("?" for _ in moved_ids)
    src.execute(
        f"DELETE FROM memory_edges WHERE source_id IN ({placeholders}) OR target_id IN ({placeholders})",
        list(moved_ids) + list(moved_ids),
    )
    src.execute(f"DELETE FROM memories WHERE id IN ({placeholders})", list(moved_ids))
    src.commit()
    src.execute("DELETE FROM memories_fts WHERE rowid NOT IN (SELECT rowid FROM memories)")
    src.commit()
    print(f"  ✓ deleted {len(moved_ids)} migrated records from antigravity")

    remaining = src.execute("SELECT COUNT(*) FROM memories").fetchone()[0]
    print(f"antigravity now holds {remaining} records")


if __name__ == "__main__":
    main()

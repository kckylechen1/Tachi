#!/usr/bin/env python3
"""Phase 1.2 — split antigravity/memory.db into project-scoped DBs.

Architect-curated path → project mapping. Records that match a destination
prefix are MOVED (copy into target DB, then deleted from source).
Records flagged as ``antigravity_keep`` stay where they are. Foundry hallucination
records were already purged in Phase 1.1.

Run:  python3 scripts/migrate_antigravity_split.py [--dry-run]

Before committing anything, every write is rehearsed in rolled-back
transactions on the source and each destination. A database this plain
``sqlite3`` connection cannot fully write is refused
before any data is committed. A rolled-back rehearsal can still create SQLite
WAL/journal files; it never commits rows. Rows with a NULL ``id`` have no stable identity to move or
delete by, so they stay in the source and are reported. Use explicit destination
paths for stores outside the legacy layout. An optional, operator-built libsimple
extension enables the production tokenizer; no extension is downloaded.

Stop all writers and back up every database before applying. Separate database
commits are NOT crash atomic. A crash can leave copies in both places; rerunning
accepts identical copies and refuses conflicting identities before any commit.
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
    return {row[1] for row in conn.execute(f"PRAGMA table_info({identifier(table)})")}


def identifier(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def json_list(raw) -> list[str]:
    try:
        value = json.loads(raw or "[]")
    except (TypeError, json.JSONDecodeError):
        return []
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, str) and item]


def entities_with_legacy_persons(rec: dict) -> str:
    entities = json.loads(rec.get("entities") or "[]")
    seen = {entity.casefold() for entity in entities}
    for person in json.loads(rec.get("persons") or "[]"):
        key = person.casefold()
        if key not in seen:
            entities.append(person)
            seen.add(key)
    return json.dumps(entities, ensure_ascii=False)


def fts_terms(raw):
    """Match memcore's db/fts_projection.rs, including its legacy fallback."""
    if raw is None:
        return None
    if not isinstance(raw, str):
        raise sqlite3.DataError("FTS terms must be stored as TEXT or NULL")
    try:
        value = json.loads(raw)
        if isinstance(value, list) and all(isinstance(item, str) for item in value):
            # serde_json rejects unpaired surrogate escapes.
            for item in value:
                item.encode("utf-8")
            return " ".join(value)
    except (ValueError, UnicodeEncodeError):
        pass
    return raw.replace("[", " ").replace("]", " ").replace('"', " ").strip(" ")


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
    sigil_markers = ("sigil", "memcore", "tachi-server", "tachi-mcp")
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
    # Current schemas have no location/persons and include additional durable
    # recall/validity fields. Never project a fixed legacy subset of the row.
    cur = conn.execute("SELECT * FROM memories")
    cols = [d[0] for d in cur.description]
    for row in cur.fetchall():
        yield dict(zip(cols, row))


def fetch_edges_for(conn: sqlite3.Connection, ids: set[str]):
    if not ids:
        return []
    placeholders = ",".join("?" for _ in ids)
    sql = (
        f"SELECT * "
        f"FROM memory_edges WHERE source_id IN ({placeholders}) OR target_id IN ({placeholders})"
    )
    cur = conn.execute(sql, list(ids) + list(ids))
    columns = [d[0] for d in cur.description]
    return [dict(zip(columns, row)) for row in cur.fetchall()]


def ensure_project_db(project: str) -> Path:
    if project == "global":
        return GLOBAL_DB
    proj_dir = PROJECTS_DIR / project
    return proj_dir / "memory.db"


def insert_record(conn: sqlite3.Connection, rec: dict):
    values = dict(rec)
    if "persons" in values and "persons" not in table_columns(conn, "memories"):
        # Retain the legacy fold only where it is lossless. Current entities
        # remain byte-for-byte unchanged, including malformed legacy JSON.
        for field in ("entities", "persons"):
            try:
                parsed = json.loads(values[field] or "[]")
                if not isinstance(parsed, list) or not all(isinstance(v, str) for v in parsed):
                    raise ValueError()
            except (TypeError, ValueError) as exc:
                raise sqlite3.DataError("cannot losslessly fold legacy persons") from exc
        values["entities"] = entities_with_legacy_persons(values)
        del values["persons"]
    insert_identical_or_new(conn, "memories", values, ("id",))


def insert_identical_or_new(conn, table: str, values: dict, key: tuple[str, ...]):
    """Preserve all source fields, or refuse before any database is committed."""
    missing = set(values) - table_columns(conn, table)
    if missing:
        raise sqlite3.DataError(f"destination {table} lacks columns: {', '.join(sorted(missing))}")
    columns = ", ".join(identifier(c) for c in values)
    where = " AND ".join(f"{identifier(c)} = ?" for c in key)
    parameters = [values[c] for c in key]
    lookup = f"SELECT {columns} FROM {identifier(table)} WHERE {where}"
    expected = tuple(values.values())
    existing = conn.execute(lookup, parameters).fetchall()
    if not existing:
        placeholders = ",".join("?" for _ in values)
        conn.execute(f"INSERT INTO {identifier(table)} ({columns}) VALUES ({placeholders})", expected)
        existing = conn.execute(lookup, parameters).fetchall()
    if len(existing) != 1 or any(
        type(a) is not type(b) or a != b for a, b in zip(existing[0], expected)
    ):
        # Do not expose private ids or contents in operator/public logs.
        raise sqlite3.IntegrityError(f"conflicting or altered {table} identity; restore/reconcile before retry")


# Standalone FTS projections keyed by the stable memory id. Their rowids are
# independent of ``memories`` and they have no sync triggers.
FTS_PROJECTIONS = ("memories_fts", "memories_symbolic_fts")


def existing_fts_projections(conn: sqlite3.Connection) -> list[str]:
    present = {
        row[0]
        for row in conn.execute(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name IN (?, ?)",
            FTS_PROJECTIONS,
        )
    }
    return [name for name in FTS_PROJECTIONS if name in present]


def write_destination(dst: sqlite3.Connection, src: sqlite3.Connection, recs: list[dict]):
    for rec in recs:
        insert_record(dst, rec)
    for edge in fetch_edges_for(src, {r["id"] for r in recs}):
        insert_edge(dst, edge)
    # Project from the inserted primary row, inside the same transaction. FTS
    # virtual tables do not support UPSERT; replace only the moved stable ids.
    for table in existing_fts_projections(dst):
        fields = ["id", "path", "summary", "text", "keywords", "entities"]
        if table == "memories_symbolic_fts":
            fields.append("topic")
        expressions = [
            f"memcore_fts_terms({identifier(field)})"
            if table == "memories_fts" and field in ("keywords", "entities") else identifier(field)
            for field in fields
        ]
        for rec in recs:
            dst.execute(f"DELETE FROM {table} WHERE id = ?", (rec["id"],))
            dst.execute(
                f"INSERT INTO {table} ({', '.join(fields)}) "
                f"SELECT {', '.join(expressions)} FROM memories WHERE id = ?", (rec["id"],)
            )


def cleanup_source(src: sqlite3.Connection, moved_ids: set[str]):
    placeholders = ",".join("?" for _ in moved_ids)
    src.execute(
        f"DELETE FROM memory_edges WHERE source_id IN ({placeholders}) OR target_id IN ({placeholders})",
        list(moved_ids) + list(moved_ids),
    )
    src.execute(f"DELETE FROM memories WHERE id IN ({placeholders})", list(moved_ids))
    # FTS rowids are independent; match stable ids and exclude NULL poison.
    for table in existing_fts_projections(src):
        src.execute(
            f"DELETE FROM {table} WHERE id IS NULL "
            "OR id NOT IN (SELECT id FROM memories WHERE id IS NOT NULL)"
        )


def rehearse(conn: sqlite3.Connection, label: str, write) -> None:
    """Run ``write`` in a transaction that is always rolled back."""
    conn.execute("SAVEPOINT split_preflight")
    try:
        write()
    except sqlite3.Error as exc:
        sys.exit(
            f"preflight: this connection cannot write {label}: {exc}; "
            "no data was committed to any database (run the cleanup through tachi-server instead)"
        )
    finally:
        conn.execute("ROLLBACK TO split_preflight")
        conn.execute("RELEASE split_preflight")


def insert_edge(conn: sqlite3.Connection, edge: dict):
    insert_identical_or_new(conn, "memory_edges", edge, ("source_id", "target_id", "relation"))


def configure_connection(conn, library):
    conn.create_function("memcore_fts_terms", 1, fts_terms, deterministic=True)
    if library is not None:
        conn.enable_load_extension(True)
        try:
            for entrypoint in ("sqlite3_simple_init", "sqlite3_simpletokenizer_init"):
                conn.execute("SELECT load_extension(?, ?)", (str(library), entrypoint))
        finally:
            conn.enable_load_extension(False)
    # Current schema guards need the function even when NEW.metadata contains
    # no reserved references. This connection can never arm typed mutation or
    # schema-migration authority; those writes must still fail closed.
    conn.create_function("tachi_reserved_reference_write_enabled", 0, lambda: 0)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--source-db", type=Path, default=SOURCE_DB)
    parser.add_argument("--destination", action="append", default=[], metavar="PROJECT=DB",
                        help="override a destination with an existing initialized database")
    parser.add_argument("--simple-tokenizer-library", type=Path,
                        help="explicit trusted operator-built libsimple shared library")
    args = parser.parse_args()

    source_path = args.source_db
    if not source_path.is_file():
        parser.error("source database must be an existing regular file")
    overrides = {}
    for option in args.destination:
        project, separator, path = option.partition("=")
        if not separator or project not in ("global", "hapi", "tachi", "sigil", "quant", "hyperion", "openclaw") or not path or project in overrides:
            parser.error("destination must be a unique known PROJECT=DB")
        overrides[project] = Path(path).resolve(strict=True)
    library = args.simple_tokenizer_library
    if library is not None:
        library = library.resolve(strict=True)
        if not library.is_file():
            parser.error("tokenizer library must be an existing regular file")

    src = sqlite3.connect(str(source_path))
    src.row_factory = sqlite3.Row
    destinations = {}
    try:
        configure_connection(src, library)
        # Pin classification, edge reads and cleanup to one source snapshot.
        # Dry-run opens a read transaction and performs no write rehearsal.
        src.execute("BEGIN" if args.dry_run else "BEGIN IMMEDIATE")
        migrate(src, source_path, overrides, destinations, args, library)
    finally:
        src.rollback()
        for dst in destinations.values():
            dst.rollback()
            dst.close()


def migrate(src, source_path, overrides, connections, args, library):

    moves: dict[str, list[dict]] = {}
    keep = 0
    null_id_retained = 0
    for rec in fetch_rows(src):
        dest = classify(rec["path"], rec["text"])
        if dest == "keep":
            keep += 1
            continue
        if rec["id"] is None:
            null_id_retained += 1
            continue
        moves.setdefault(dest, []).append(rec)

    print("=== Migration plan ===")
    for k, v in sorted(moves.items(), key=lambda kv: -len(kv[1])):
        print(f"  → {k:10s} : {len(v)} records")
    print(f"  → keep      : {keep} records (stay in antigravity)")
    print(f"  → null id   : {null_id_retained} records (no stable id; stay in antigravity)")
    total = sum(len(v) for v in moves.values()) + keep + null_id_retained
    print(f"  TOTAL       : {total}")

    if args.dry_run:
        print("dry-run: no writes")
        return

    moved_ids: set[str] = {rec["id"] for recs in moves.values() for rec in recs}
    destinations = {project: overrides.get(project, ensure_project_db(project)) for project in moves}
    identities = {(source_path.stat().st_dev, source_path.stat().st_ino)}
    for dest_path in destinations.values():
        if not dest_path.is_file():
            sys.exit(f"target DB missing (must be initialised by tachi-server first): {dest_path}")
        identity = (dest_path.stat().st_dev, dest_path.stat().st_ino)
        if identity in identities:
            sys.exit("preflight: source and destinations must be distinct physical databases")
        identities.add(identity)

    # Rehearse every write before committing any of them. Destinations and the
    # source are separate databases, so a failure after the first commit could
    # otherwise leave copies in a destination with the source untouched.
    for project, dest_path in destinations.items():
        dst = sqlite3.connect(str(dest_path))
        connections[project] = dst
        configure_connection(dst, library)
        dst.execute("BEGIN IMMEDIATE")
        rehearse(dst, str(dest_path), lambda: write_destination(dst, src, moves[project]))
    rehearse(src, str(source_path), lambda: cleanup_source(src, moved_ids))

    for project, dest_path in destinations.items():
        recs = moves[project]
        dst = connections[project]
        try:
            write_destination(dst, src, recs)
            dst.commit()
            print(f"  ✓ wrote {len(recs)} → {dest_path}")
        except Exception as exc:
            dst.rollback()
            sys.exit(f"failed writing to {dest_path}: {exc}")

    # Delete migrated rows from source.
    cleanup_source(src, moved_ids)
    src.commit()
    print(f"  ✓ deleted {len(moved_ids)} migrated records from antigravity")

    remaining = src.execute("SELECT COUNT(*) FROM memories").fetchone()[0]
    print(f"antigravity now holds {remaining} records")


if __name__ == "__main__":
    main()

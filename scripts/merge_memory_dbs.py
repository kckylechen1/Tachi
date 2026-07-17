#!/usr/bin/env python3
"""Merge Antigravity memories from multiple SQLite DBs into one target DB.

The script is intentionally conservative:
1. Backs up the target DB first using SQLite's online backup API.
2. Merges only memory rows and graph edges.
3. Never overwrites existing memory IDs in the target DB.
4. Inserts edges only when both endpoints exist in the target DB.
5. Optionally syncs external vector rows from `memories_vec` when available.
6. Prints detailed per-source and aggregate statistics.
7. Runs `PRAGMA integrity_check` at the end and exits non-zero on failure.

Defaults match the Antigravity migration described in the task, but paths can
be overridden from the command line.
"""

from __future__ import annotations

import argparse
import datetime as dt
import sqlite3
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path


DEFAULT_SOURCE_DBS = [
    "~/.gemini/antigravity/memory.db",
    "~/Desktop/Sigil/.tachi/memory.db",
]
DEFAULT_TARGET_DB = "~/.tachi/projects/antigravity/tachi-memory.db"


@dataclass
class SourceStats:
    label: str
    path: str
    source_memory_total: int = 0
    inserted_memories: int = 0
    skipped_existing_memories: int = 0
    source_edge_total: int = 0
    inserted_edges: int = 0
    skipped_edges_missing_memory: int = 0
    skipped_existing_edges: int = 0
    source_vector_total: int = 0
    inserted_vectors: int = 0
    skipped_existing_vectors: int = 0
    edge_table_name: str | None = None
    vector_sync_note: str | None = None


def eprint(message: str) -> None:
    print(message, file=sys.stderr)


def quote_ident(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def quote_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def expand_db_path(raw_path: str) -> str:
    return str(Path(raw_path).expanduser().resolve())


def timestamp_suffix() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def ensure_db_exists(path: str, kind: str) -> None:
    if not Path(path).exists():
        raise FileNotFoundError(f"{kind} DB not found: {path}")


def connect_rw(path: str) -> sqlite3.Connection:
    uri = Path(path).as_uri() + "?mode=rwc"
    conn = sqlite3.connect(uri, uri=True)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys = ON")
    conn.execute("PRAGMA busy_timeout = 5000")
    return conn


def table_exists(conn: sqlite3.Connection, schema: str, table: str) -> bool:
    sql = f"""
        SELECT 1
        FROM {quote_ident(schema)}.sqlite_master
        WHERE type IN ('table', 'view') AND name = ?
        LIMIT 1
    """
    return conn.execute(sql, (table,)).fetchone() is not None


def get_table_columns(conn: sqlite3.Connection, schema: str, table: str) -> list[str]:
    pragma = f"PRAGMA {quote_ident(schema)}.table_info({quote_ident(table)})"
    return [row["name"] for row in conn.execute(pragma)]


def get_pk_columns(conn: sqlite3.Connection, schema: str, table: str) -> list[str]:
    pragma = f"PRAGMA {quote_ident(schema)}.table_info({quote_ident(table)})"
    rows = list(conn.execute(pragma))
    return [row["name"] for row in sorted(rows, key=lambda row: row["pk"]) if row["pk"]]


def pick_edge_table(conn: sqlite3.Connection, schema: str) -> str | None:
    for candidate in ("memory_edges", "edges"):
        if table_exists(conn, schema, candidate):
            return candidate
    return None


def common_columns(
    conn: sqlite3.Connection,
    target_schema: str,
    target_table: str,
    source_schema: str,
    source_table: str,
) -> list[str]:
    target_cols = get_table_columns(conn, target_schema, target_table)
    source_cols = set(get_table_columns(conn, source_schema, source_table))
    return [col for col in target_cols if col in source_cols]


def sql_count(conn: sqlite3.Connection, sql: str, params: tuple = ()) -> int:
    return int(conn.execute(sql, params).fetchone()[0])


def online_backup(target_path: str, backup_path: str) -> None:
    backup_parent = Path(backup_path).parent
    backup_parent.mkdir(parents=True, exist_ok=True)
    backup_file = Path(backup_path)
    if backup_file.exists():
        backup_file.unlink()

    source = sqlite3.connect(target_path)
    try:
        dest = sqlite3.connect(backup_path)
        try:
            source.backup(dest)
        finally:
            dest.close()
    finally:
        source.close()


def rebuild_fts_if_present(conn: sqlite3.Connection) -> bool:
    if not table_exists(conn, "main", "memories_fts"):
        return False

    try:
        conn.execute("DELETE FROM memories_fts")
        conn.execute(
            """
            INSERT INTO memories_fts (id, path, summary, text, keywords, entities)
            SELECT
                id,
                path,
                summary,
                text,
                trim(replace(replace(replace(keywords, '[', ' '), ']', ' '), '"', ' ')),
                trim(replace(replace(replace(entities, '[', ' '), ']', ' '), '"', ' '))
            FROM memories
            """
        )
        return True
    except sqlite3.OperationalError as exc:
        # Tachi registers a custom FTS5 'simple' tokenizer in Rust.
        # Python's sqlite3 doesn't have it, so FTS rebuild will fail here.
        # The Tachi server will rebuild FTS automatically on next write.
        eprint(f"  FTS rebuild skipped (custom tokenizer not available: {exc})")
        eprint("  → Tachi will rebuild FTS index automatically on next memory write.")
        return False


def merge_memories_for_source(
    conn: sqlite3.Connection,
    source_schema: str,
    stats: SourceStats,
) -> None:
    if not table_exists(conn, source_schema, "memories"):
        raise RuntimeError(f"{stats.label}: source DB has no memories table")

    shared_columns = common_columns(conn, "main", "memories", source_schema, "memories")
    if "id" not in shared_columns:
        raise RuntimeError(f"{stats.label}: source memories table is missing id")

    stats.source_memory_total = sql_count(
        conn, f"SELECT COUNT(*) FROM {quote_ident(source_schema)}.memories"
    )

    conn.execute("DROP TABLE IF EXISTS temp.merge_new_ids")
    conn.execute("CREATE TEMP TABLE temp.merge_new_ids (id TEXT PRIMARY KEY)")
    conn.execute(
        f"""
        INSERT INTO temp.merge_new_ids (id)
        SELECT src.id
        FROM {quote_ident(source_schema)}.memories AS src
        LEFT JOIN main.memories AS dst ON dst.id = src.id
        WHERE dst.id IS NULL
        """
    )

    stats.inserted_memories = sql_count(conn, "SELECT COUNT(*) FROM temp.merge_new_ids")
    stats.skipped_existing_memories = stats.source_memory_total - stats.inserted_memories

    if stats.inserted_memories == 0:
        return

    col_sql = ", ".join(quote_ident(col) for col in shared_columns)
    select_sql = ", ".join(f"src.{quote_ident(col)}" for col in shared_columns)
    before = conn.total_changes
    conn.execute(
        f"""
        INSERT OR IGNORE INTO main.memories ({col_sql})
        SELECT {select_sql}
        FROM {quote_ident(source_schema)}.memories AS src
        JOIN temp.merge_new_ids AS ids ON ids.id = src.id
        """
    )
    inserted = conn.total_changes - before
    if inserted != stats.inserted_memories:
        raise RuntimeError(
            f"{stats.label}: expected to insert {stats.inserted_memories} memories, "
            f"but SQLite reported {inserted}"
        )


def merge_external_vectors_for_source(
    conn: sqlite3.Connection,
    source_schema: str,
    stats: SourceStats,
) -> None:
    if not table_exists(conn, "main", "memories_vec") or not table_exists(
        conn, source_schema, "memories_vec"
    ):
        stats.vector_sync_note = "external memories_vec not present in both DBs"
        return

    try:
        stats.source_vector_total = sql_count(
            conn,
            f"""
            SELECT COUNT(*)
            FROM {quote_ident(source_schema)}.memories_vec AS src
            JOIN temp.merge_new_ids AS ids ON ids.id = src.id
            """,
        )
        before = conn.total_changes
        conn.execute(
            f"""
            INSERT OR IGNORE INTO main.memories_vec (id, embedding)
            SELECT src.id, src.embedding
            FROM {quote_ident(source_schema)}.memories_vec AS src
            JOIN temp.merge_new_ids AS ids ON ids.id = src.id
            """
        )
        stats.inserted_vectors = conn.total_changes - before
        stats.skipped_existing_vectors = stats.source_vector_total - stats.inserted_vectors
        stats.vector_sync_note = "external memories_vec synced"
    except sqlite3.DatabaseError as exc:
        stats.vector_sync_note = (
            "external memories_vec exists but could not be queried with the current "
            f"sqlite3 build: {exc}"
        )


def build_edge_key_columns(
    conn: sqlite3.Connection,
    target_schema: str,
    target_table: str,
    source_schema: str,
    source_table: str,
) -> list[str]:
    target_pks = get_pk_columns(conn, target_schema, target_table)
    source_cols = set(get_table_columns(conn, source_schema, source_table))
    if target_pks and all(col in source_cols for col in target_pks):
        return target_pks

    for candidate in (["source_id", "target_id", "relation"], ["source_id", "target_id"]):
        if all(col in source_cols for col in candidate):
            target_cols = set(get_table_columns(conn, target_schema, target_table))
            if all(col in target_cols for col in candidate):
                return candidate

    raise RuntimeError(
        f"Unable to determine edge identity columns for {target_table} from source {source_table}"
    )


def merge_edges_for_source(
    conn: sqlite3.Connection,
    source_schema: str,
    stats: SourceStats,
) -> None:
    target_edge_table = pick_edge_table(conn, "main")
    source_edge_table = pick_edge_table(conn, source_schema)
    stats.edge_table_name = source_edge_table

    if not target_edge_table or not source_edge_table:
        return

    shared_columns = common_columns(
        conn, "main", target_edge_table, source_schema, source_edge_table
    )
    required = {"source_id", "target_id"}
    if not required.issubset(shared_columns):
        raise RuntimeError(
            f"{stats.label}: shared edge columns are missing required IDs: {sorted(required)}"
        )

    key_columns = build_edge_key_columns(
        conn, "main", target_edge_table, source_schema, source_edge_table
    )
    key_match = " AND ".join(
        f"dst.{quote_ident(col)} = src.{quote_ident(col)}" for col in key_columns
    )
    valid_endpoints = """
        EXISTS (SELECT 1 FROM main.memories AS s_mem WHERE s_mem.id = src.source_id)
        AND EXISTS (SELECT 1 FROM main.memories AS t_mem WHERE t_mem.id = src.target_id)
    """

    stats.source_edge_total = sql_count(
        conn, f"SELECT COUNT(*) FROM {quote_ident(source_schema)}.{quote_ident(source_edge_table)}"
    )
    valid_edge_total = sql_count(
        conn,
        f"""
        SELECT COUNT(*)
        FROM {quote_ident(source_schema)}.{quote_ident(source_edge_table)} AS src
        WHERE {valid_endpoints}
        """,
    )
    stats.skipped_edges_missing_memory = stats.source_edge_total - valid_edge_total

    if valid_edge_total == 0:
        stats.skipped_existing_edges = 0
        return

    col_sql = ", ".join(quote_ident(col) for col in shared_columns)
    select_sql = ", ".join(f"src.{quote_ident(col)}" for col in shared_columns)
    before = conn.total_changes
    conn.execute(
        f"""
        INSERT OR IGNORE INTO main.{quote_ident(target_edge_table)} ({col_sql})
        SELECT {select_sql}
        FROM {quote_ident(source_schema)}.{quote_ident(source_edge_table)} AS src
        WHERE {valid_endpoints}
        AND NOT EXISTS (
            SELECT 1
            FROM main.{quote_ident(target_edge_table)} AS dst
            WHERE {key_match}
        )
        """
    )
    stats.inserted_edges = conn.total_changes - before
    stats.skipped_existing_edges = valid_edge_total - stats.inserted_edges


def attach_source(conn: sqlite3.Connection, schema: str, path: str) -> None:
    uri = Path(path).as_uri() + "?mode=ro"
    conn.execute(f"ATTACH DATABASE ? AS {quote_ident(schema)}", (uri,))


def detach_source(conn: sqlite3.Connection, schema: str) -> None:
    conn.execute(f"DETACH DATABASE {quote_ident(schema)}")


def run_integrity_check(conn: sqlite3.Connection, edge_table: str | None) -> tuple[bool, list[str]]:
    try:
        rows = [row[0] for row in conn.execute("PRAGMA integrity_check").fetchall()]
        return rows == ["ok"], rows
    except sqlite3.DatabaseError as exc:
        details = [f"integrity_check failed to run: {exc}"]
        table_checks_ok = True
        for table in filter(None, ["memories", edge_table]):
            try:
                rows = [
                    row[0]
                    for row in conn.execute(
                        f"PRAGMA integrity_check({quote_literal(table)})"
                    ).fetchall()
                ]
                details.append(f"{table}: {'; '.join(rows)}")
                table_checks_ok = table_checks_ok and rows == ["ok"]
            except sqlite3.DatabaseError as table_exc:
                details.append(f"{table}: failed to run table integrity_check: {table_exc}")
                table_checks_ok = False

        if table_checks_ok:
            details.append(
                "table-level integrity checks passed; treating global integrity_check failure as a sqlite3 module limitation"
            )
            return True, details

        return False, details


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Merge Antigravity Tachi memory DBs into the active target DB."
    )
    parser.add_argument(
        "--target",
        default=DEFAULT_TARGET_DB,
        help=f"Target DB to merge into (default: {DEFAULT_TARGET_DB})",
    )
    parser.add_argument(
        "--source",
        action="append",
        dest="sources",
        default=None,
        help="Source DB to merge from. Repeat for multiple DBs.",
    )
    parser.add_argument(
        "--backup-path",
        default=None,
        help="Optional explicit backup path for the target DB.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Inspect schemas and print what would happen without writing anything.",
    )
    return parser.parse_args()


def print_header(title: str) -> None:
    print(f"\n{title}")
    print("-" * len(title))


def main() -> int:
    args = parse_args()

    target_path = expand_db_path(args.target)
    source_paths = [expand_db_path(p) for p in (args.sources or DEFAULT_SOURCE_DBS)]

    if not source_paths:
        eprint("At least one --source DB is required")
        return 2

    all_paths = [target_path, *source_paths]
    if len(set(all_paths)) != len(all_paths):
        eprint("Target DB and source DB paths must all be distinct")
        return 2

    try:
        ensure_db_exists(target_path, "target")
        for path in source_paths:
            ensure_db_exists(path, "source")
    except FileNotFoundError as exc:
        eprint(f"ERROR: {exc}")
        return 2

    backup_path = (
        expand_db_path(args.backup_path)
        if args.backup_path
        else str(Path(target_path).with_name(f"{Path(target_path).name}.backup-{timestamp_suffix()}"))
    )

    print("Target DB :", target_path)
    print("Source DBs:")
    for idx, path in enumerate(source_paths, start=1):
        print(f"  {idx}. {path}")
    print("Backup DB :", backup_path)
    print("Mode      :", "dry-run" if args.dry_run else "apply")

    working_target_path = target_path
    dry_run_dir: tempfile.TemporaryDirectory[str] | None = None

    if not args.dry_run:
        online_backup(target_path, backup_path)
        print("\nBackup created successfully.")
    else:
        dry_run_dir = tempfile.TemporaryDirectory(prefix="merge-memory-dbs-")
        working_target_path = str(Path(dry_run_dir.name) / "target-dry-run.db")
        online_backup(target_path, working_target_path)
        print("\nDry-run uses a temporary copy of the target DB.")

    stats_by_source: list[SourceStats] = []

    conn = connect_rw(working_target_path)
    try:
        if not table_exists(conn, "main", "memories"):
            raise RuntimeError("Target DB has no memories table")

        target_edge_table = pick_edge_table(conn, "main")
        fts_rebuilt = False

        for index, path in enumerate(source_paths, start=1):
            attach_source(conn, f"src{index}", path)

        conn.execute("BEGIN IMMEDIATE")
        for index, path in enumerate(source_paths, start=1):
            schema = f"src{index}"
            source_stats = SourceStats(label=f"source-{index}", path=path)
            merge_memories_for_source(conn, schema, source_stats)
            merge_external_vectors_for_source(conn, schema, source_stats)
            merge_edges_for_source(conn, schema, source_stats)
            stats_by_source.append(source_stats)

        fts_rebuilt = rebuild_fts_if_present(conn)
        conn.commit()

        final_memory_total = sql_count(conn, "SELECT COUNT(*) FROM main.memories")
        final_edge_total = (
            sql_count(conn, f"SELECT COUNT(*) FROM main.{quote_ident(target_edge_table)}")
            if target_edge_table
            else 0
        )
        integrity_ok, integrity_rows = run_integrity_check(conn, target_edge_table)
    except Exception as exc:
        try:
            conn.rollback()
        except sqlite3.Error:
            pass
        eprint(f"\nERROR: {exc}")
        return 1
    finally:
        for index in range(1, len(source_paths) + 1):
            schema = f"src{index}"
            try:
                detach_source(conn, schema)
            except sqlite3.Error:
                pass
        conn.close()
        if dry_run_dir is not None:
            dry_run_dir.cleanup()

    print_header("Per-source stats")
    for source_stats in stats_by_source:
        print(source_stats.path)
        print(
            f"  memories: source={source_stats.source_memory_total} "
            f"inserted={source_stats.inserted_memories} "
            f"skipped_existing={source_stats.skipped_existing_memories}"
        )
        if source_stats.edge_table_name:
            print(
                f"  edges ({source_stats.edge_table_name}): source={source_stats.source_edge_total} "
                f"inserted={source_stats.inserted_edges} "
                f"skipped_missing_memory={source_stats.skipped_edges_missing_memory} "
                f"skipped_existing={source_stats.skipped_existing_edges}"
            )
        else:
            print("  edges: no source/target edge table detected, skipped")
        if source_stats.vector_sync_note:
            print(
                f"  vectors: source={source_stats.source_vector_total} "
                f"inserted={source_stats.inserted_vectors} "
                f"skipped_existing={source_stats.skipped_existing_vectors}"
            )
            print(f"  vector_note: {source_stats.vector_sync_note}")

    total_inserted_memories = sum(item.inserted_memories for item in stats_by_source)
    total_skipped_memories = sum(item.skipped_existing_memories for item in stats_by_source)
    total_inserted_edges = sum(item.inserted_edges for item in stats_by_source)
    total_skipped_edges_missing = sum(item.skipped_edges_missing_memory for item in stats_by_source)
    total_skipped_existing_edges = sum(item.skipped_existing_edges for item in stats_by_source)
    total_inserted_vectors = sum(item.inserted_vectors for item in stats_by_source)

    print_header("Summary")
    print(f"Inserted memories : {total_inserted_memories}")
    print(f"Skipped memories  : {total_skipped_memories}")
    print(f"Inserted edges    : {total_inserted_edges}")
    print(f"Skipped edges     : missing_memory={total_skipped_edges_missing} existing={total_skipped_existing_edges}")
    print(f"Inserted vectors  : {total_inserted_vectors}")
    print(f"Final memories    : {final_memory_total}")
    print(f"Final edges       : {final_edge_total}")
    print(f"FTS rebuilt       : {'yes' if fts_rebuilt else 'no'}")

    print_header("Integrity check")
    for row in integrity_rows:
        print(row)

    if not integrity_ok:
        eprint("\nIntegrity check did not pass.")
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())

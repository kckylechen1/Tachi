"""Scratch-database regressions for the legacy migration's source cleanup."""

from contextlib import closing, redirect_stdout
import importlib.util
import io
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("migrate_antigravity_split.py")
SCHEMA = """
CREATE TABLE memories (
    id TEXT PRIMARY KEY, path TEXT, summary TEXT DEFAULT '', text TEXT DEFAULT 'neutral fixture',
    importance REAL DEFAULT 0.5, timestamp TEXT DEFAULT '', category TEXT DEFAULT 'fact',
    topic TEXT DEFAULT '', keywords TEXT DEFAULT '[]', entities TEXT DEFAULT '[]',
    location TEXT DEFAULT '', source TEXT DEFAULT 'manual', scope TEXT DEFAULT 'project',
    archived INTEGER DEFAULT 0, created_at TEXT DEFAULT '', updated_at TEXT DEFAULT '',
    access_count INTEGER DEFAULT 0, last_access TEXT, revision INTEGER DEFAULT 1,
    metadata TEXT DEFAULT '{}', retention_policy TEXT, domain TEXT
);
CREATE TABLE memory_edges (
    source_id TEXT, target_id TEXT, relation TEXT, weight REAL DEFAULT 0.5,
    metadata TEXT DEFAULT '{}', created_at TEXT DEFAULT '', valid_from TEXT DEFAULT '', valid_to TEXT,
    PRIMARY KEY (source_id, target_id, relation)
);
"""


class MigrationCleanupTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "projects/antigravity/memory.db"
        self.target = self.root / "projects/tachi/memory.db"
        self.original_connect = sqlite3.connect
        self.opened = []
        self.addCleanup(self.close_connections)
        spec = importlib.util.spec_from_file_location("migration_under_test", SCRIPT)
        self.script = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.script)
        for path in [self.source, self.target]:
            path.parent.mkdir(parents=True, exist_ok=True)
            with closing(self.original_connect(path)) as conn:
                conn.executescript(SCHEMA)
        with closing(self.original_connect(self.source)) as conn:
            conn.executemany("INSERT INTO memories(rowid, id, path) VALUES (?, ?, ?)", [
                (1, "keep", "/antigravity/keep"), (2, "move", "/tachi/move"),
                (3, None, "/antigravity/null-a"), (4, None, "/antigravity/null-b"),
            ])
            conn.executemany("INSERT INTO memory_edges(source_id, target_id, relation) VALUES (?, ?, 'related')", [
                ("keep", "keep"), ("move", "keep"),
            ])
            conn.execute("CREATE VIRTUAL TABLE memories_fts USING fts5(id UNINDEXED, path, summary, text, keywords, entities)")
            # None of these rowids express the identity of the projected memory.
            conn.executemany("INSERT INTO memories_fts(rowid, id, text) VALUES (?, ?, ?)", [
                (77, "keep", "live projection"), (1, "move", "moved projection"),
                (3, "orphan", "orphan projection"), (4, None, "null projection"),
            ])
            conn.commit()

    def close_connections(self):
        for conn in self.opened:
            conn.close()

    def rows(self, path, sql):
        with closing(self.original_connect(path)) as conn:
            return conn.execute(sql).fetchall()

    def source_snapshot(self):
        return [self.rows(self.source, sql) for sql in [
            "SELECT rowid, * FROM memories ORDER BY rowid",
            "SELECT * FROM memory_edges ORDER BY source_id, target_id",
            "SELECT rowid, * FROM memories_fts ORDER BY rowid",
        ]]

    def run_main(self, *, dry_run=False, source_authorizer=None):
        def connect(database, *args, **kwargs):
            conn = self.original_connect(database, *args, **kwargs)
            self.opened.append(conn)
            if source_authorizer is not None and Path(database) == self.source:
                conn.set_authorizer(source_authorizer)
            return conn

        # Override every home-derived path before invoking the actual script.
        with mock.patch.multiple(self.script, TACHI_ROOT=self.root, SOURCE_DB=self.source,
                                 GLOBAL_DB=self.root / "global/memory.db", PROJECTS_DIR=self.root / "projects"), \
             mock.patch.object(sys, "argv", [str(SCRIPT)] + (["--dry-run"] if dry_run else [])), \
             mock.patch.object(self.script.sqlite3, "connect", connect), redirect_stdout(io.StringIO()):
            self.script.main()

    def test_cleanup_uses_stable_ids_and_removes_null_and_orphan_projections(self):
        self.run_main()
        self.assertEqual(self.rows(self.source, "SELECT id FROM memories ORDER BY rowid"), [("keep",), (None,), (None,)])
        self.assertEqual(self.rows(self.source, "SELECT rowid, id, text FROM memories_fts ORDER BY rowid"), [(77, "keep", "live projection")])
        self.assertEqual(self.rows(self.source, "SELECT source_id, target_id FROM memory_edges"), [("keep", "keep")])
        self.assertEqual(self.rows(self.target, "SELECT id, path FROM memories"), [("move", "/tachi/move")])
        self.assertEqual(self.rows(self.target, "SELECT source_id, target_id FROM memory_edges"), [("move", "keep")])

    def test_dry_run_preserves_source_and_destination(self):
        before = self.source_snapshot()
        self.run_main(dry_run=True)
        self.assertEqual(self.source_snapshot(), before)
        self.assertEqual(self.rows(self.target, "SELECT * FROM memories"), [])
        self.assertEqual(self.rows(self.target, "SELECT * FROM memory_edges"), [])

    def test_fts_delete_failure_rolls_back_source_cleanup(self):
        before = self.source_snapshot()
        deletes = []

        def deny_fts_delete(action, table, _column, _database, _trigger):
            if action == sqlite3.SQLITE_DELETE:
                deletes.append(table)
                if table == "memories_fts":
                    return sqlite3.SQLITE_DENY
            return sqlite3.SQLITE_OK

        with self.assertRaisesRegex(sqlite3.DatabaseError, "not authorized"):
            self.run_main(source_authorizer=deny_fts_delete)
        self.assertLess(deletes.index("memory_edges"), deletes.index("memories"))
        self.assertLess(deletes.index("memories"), deletes.index("memories_fts"))
        self.assertEqual(self.source_snapshot(), before)
        self.assertFalse(self.opened[0].in_transaction)
        # Destinations are committed separately; this is source atomicity only.
        self.assertEqual(self.rows(self.target, "SELECT id FROM memories"), [("move",)])


if __name__ == "__main__":
    unittest.main()

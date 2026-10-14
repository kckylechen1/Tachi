"""Scratch-database regressions for the legacy migration's source cleanup."""

from contextlib import closing, redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("migrate_antigravity_split.py")
GUARD_NAMES = ("memories_reserved_refs_insert_guard", "memories_reserved_refs_update_guard")
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

    def source_storage_snapshot(self):
        # Read every FTS backing table without connecting its virtual table.
        # Older SQLite versions load the tokenizer at virtual-table connect,
        # so even a SELECT would fail after installing the production DDL.
        tables = ["memories", "memory_edges"] + [
            "memories_fts_" + suffix
            for suffix in ["data", "idx", "content", "docsize", "config"]
        ]
        with closing(self.original_connect(self.source)) as conn:
            snapshot = {}
            for table in tables:
                columns = conn.execute(f'PRAGMA table_info("{table}")').fetchall()
                self.assertTrue(columns, f"missing fixture storage table: {table}")
                projection = "rowid, *" if table == "memories" else "*"
                order = "rowid" if table == "memories" else ", ".join(
                    str(i) for i in range(1, len(columns) + 1)
                )
                snapshot[table] = conn.execute(
                    f'SELECT {projection} FROM "{table}" ORDER BY {order}'
                ).fetchall()
            return snapshot

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

    def test_fts_delete_failure_refuses_before_any_write(self):
        before = self.source_snapshot()
        deletes = []

        def deny_fts_delete(action, table, _column, _database, _trigger):
            if action == sqlite3.SQLITE_DELETE:
                deletes.append(table)
                if table == "memories_fts":
                    return sqlite3.SQLITE_DENY
            return sqlite3.SQLITE_OK

        with self.assertRaisesRegex(SystemExit, "preflight: .*not authorized"):
            self.run_main(source_authorizer=deny_fts_delete)
        self.assertLess(deletes.index("memory_edges"), deletes.index("memories"))
        self.assertLess(deletes.index("memories"), deletes.index("memories_fts"))
        self.assertEqual(self.source_snapshot(), before)
        self.assertFalse(self.opened[0].in_transaction)
        self.assertEqual(self.rows(self.target, "SELECT id FROM memories"), [])

    def test_fts_delete_failure_after_preflight_rolls_back_source_cleanup(self):
        before = self.source_snapshot()
        original_cleanup = self.script.cleanup_source
        calls = []

        def deny_fts_delete(action, table, _column, _database, _trigger):
            if action == sqlite3.SQLITE_DELETE and table == "memories_fts":
                return sqlite3.SQLITE_DENY
            return sqlite3.SQLITE_OK

        def cleanup_failing_after_rehearsal(src, moved_ids):
            calls.append(moved_ids)
            if len(calls) == 2:
                # A fault the rehearsal could not foresee. Installing an
                # authorizer expires cached statements, so it applies here.
                src.set_authorizer(deny_fts_delete)
            return original_cleanup(src, moved_ids)

        with mock.patch.object(self.script, "cleanup_source", cleanup_failing_after_rehearsal), \
             self.assertRaisesRegex(sqlite3.DatabaseError, "not authorized"):
            self.run_main()
        self.assertEqual(len(calls), 2, "rehearsal, then the real cleanup")
        self.assertEqual(self.source_snapshot(), before)
        self.assertFalse(self.opened[0].in_transaction)
        # Only a failure the rehearsal could not foresee reaches this point;
        # destinations commit separately, so this is source atomicity only.
        self.assertEqual(self.rows(self.target, "SELECT id FROM memories"), [("move",)])

    def test_null_id_rows_classified_to_move_are_retained(self):
        with closing(self.original_connect(self.source)) as conn:
            conn.execute("INSERT INTO memories(rowid, id, path) VALUES (5, NULL, '/tachi/null-move')")
            conn.commit()
        self.run_main()
        self.assertEqual(
            self.rows(self.source, "SELECT rowid, path FROM memories WHERE id IS NULL ORDER BY rowid"),
            [(3, "/antigravity/null-a"), (4, "/antigravity/null-b"), (5, "/tachi/null-move")],
        )
        self.assertEqual(self.rows(self.target, "SELECT id, path FROM memories"), [("move", "/tachi/move")])
        # A rerun neither duplicates the retained row nor moves it.
        self.run_main()
        self.assertEqual(self.rows(self.target, "SELECT id, path FROM memories"), [("move", "/tachi/move")])
        self.assertEqual(self.rows(self.source, "SELECT count(*) FROM memories WHERE id IS NULL"), [(3,)])

    def test_symbolic_projection_is_cleaned_with_the_primary_projection(self):
        with closing(self.original_connect(self.source)) as conn:
            conn.execute(
                "CREATE VIRTUAL TABLE memories_symbolic_fts USING fts5("
                "id, path, summary, text, keywords, entities, topic, tokenize = 'trigram case_sensitive 0')"
            )
            conn.executemany("INSERT INTO memories_symbolic_fts(rowid, id, text) VALUES (?, ?, ?)", [
                (90, "keep", "live symbolic"), (1, "move", "moved symbolic"),
                (3, "orphan", "orphan symbolic"), (4, None, "null symbolic"),
            ])
            conn.commit()
        self.run_main()
        self.assertEqual(
            self.rows(self.source, "SELECT rowid, id, text FROM memories_symbolic_fts ORDER BY rowid"),
            [(90, "keep", "live symbolic")],
        )
        self.assertEqual(self.rows(self.source, "SELECT rowid, id, text FROM memories_fts ORDER BY rowid"), [(77, "keep", "live projection")])

    def test_unloadable_production_tokenizer_refuses_before_any_write(self):
        # Production DDL declares tokenize = 'simple' (libsimple, registered
        # only by the Rust store). Capture storage before poisoning the DDL;
        # verification must not instantiate the unavailable virtual table.
        before = self.source_storage_snapshot()
        with closing(self.original_connect(self.source)) as conn:
            conn.execute("PRAGMA writable_schema = ON")
            conn.execute(
                "UPDATE sqlite_schema SET sql = replace(sql, 'entities)', 'entities, tokenize = ''simple'')') "
                "WHERE name = 'memories_fts'"
            )
            conn.commit()
        with self.assertRaisesRegex(SystemExit, "preflight: .*no such tokenizer: simple"):
            self.run_main()
        self.assertEqual(self.source_storage_snapshot(), before)
        self.assertEqual(self.rows(self.target, "SELECT * FROM memories"), [])
        self.assertEqual(self.rows(self.target, "SELECT * FROM memory_edges"), [])

    def test_legacy_target_cannot_create_reserved_reference_metadata(self):
        with closing(self.original_connect(self.source)) as conn:
            conn.execute("UPDATE memories SET metadata=? WHERE id='move'", ('{"source_refs":[]}',))
            conn.commit()
        before = self.source_snapshot()
        with self.assertRaisesRegex(SystemExit, 'preflight: .*requires typed mutation'):
            self.run_main()
        self.assertEqual(self.source_snapshot(), before)
        self.assertEqual(self.rows(self.target, 'SELECT * FROM memories'), [])


class Schema39MigrationTest(unittest.TestCase):
    close_connections = MigrationCleanupTest.close_connections
    rows = MigrationCleanupTest.rows
    source_snapshot = MigrationCleanupTest.source_snapshot
    run_main = MigrationCleanupTest.run_main

    def setUp(self):
        MigrationCleanupTest.setUp(self)
        required = json.loads((SCRIPT.parent.parent / 'crates/memcore/src/db/schema/goldens/required-v39.json').read_text())["39"]["PortableKernel"]["required"]
        for path in (self.source, self.target):
            with closing(self.original_connect(path)) as conn:
                conn.execute("ALTER TABLE memories DROP COLUMN location")
                for declaration in (
                    "valid_from TEXT DEFAULT ''", "valid_until TEXT", "superseded_by TEXT",
                    "idless_identity TEXT", "recall_count INTEGER DEFAULT 0",
                    "query_diversity INTEGER DEFAULT 0", "tier TEXT DEFAULT 'raw'",
                    "scored_count INTEGER DEFAULT 0", "last_use_at TEXT", "extra_payload BLOB",
                ):
                    conn.execute("ALTER TABLE memories ADD COLUMN " + declaration)
                if path == self.target:
                    conn.execute("CREATE VIRTUAL TABLE memories_fts USING fts5(id UNINDEXED, path, summary, text, keywords, entities)")
                conn.execute("CREATE VIRTUAL TABLE memories_symbolic_fts USING fts5(id, path, summary, text, keywords, entities, topic, tokenize='trigram case_sensitive 0')")
                conn.execute("INSERT INTO memories_symbolic_fts(rowid,id,text) VALUES (91,'keep','symbolic keeper')")
                conn.execute("PRAGMA user_version=39")
                for name in GUARD_NAMES:
                    conn.execute(required["trigger:"+name]["sql"])
                conn.commit()
        with closing(self.original_connect(self.source)) as conn:
            conn.execute("""UPDATE memories SET valid_from='2026-10-01',valid_until='2026-11-01',
                superseded_by='successor',idless_identity='durable-identity',recall_count=17,
                query_diversity=9,tier='confirmed',scored_count=23,last_use_at='2026-10-12',
                keywords=?,entities=?,extra_payload=? WHERE id='move'""", (
                    '["two words", "quoted \\"token\\"", "slash\\\\path", "line\\nbreak", "中文"]',
                    '["Captain", "two entities"]', b'\x00\x01\xff',
                ))
            conn.commit()

    def snapshot(self, path):
        return [self.rows(path, sql) for sql in (
            "SELECT rowid,* FROM memories ORDER BY rowid",
            "SELECT * FROM memory_edges ORDER BY source_id,target_id,relation",
            "SELECT rowid,* FROM memories_fts ORDER BY rowid",
            "SELECT rowid,* FROM memories_symbolic_fts ORDER BY rowid",
        )]

    def storage_snapshot(self, path):
        # Include present FTS backing tables and DDL even when one projection
        # or guard is deliberately missing. Refusal must repair nothing.
        with closing(self.original_connect(path)) as conn:
            schema = conn.execute('SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY name').fetchall()
            tables = {}
            for name, in conn.execute("SELECT name FROM sqlite_schema WHERE type='table'").fetchall():
                projection = 'rowid,*' if name == 'memories' else '*'
                tables[name] = sorted(conn.execute('SELECT '+projection+' FROM "'+name+'"').fetchall(), key=repr)
            return schema, tables

    def test_current_fields_and_both_destination_projections_survive(self):
        expected = self.rows(self.source, "SELECT * FROM memories WHERE id='move'")
        self.run_main()
        self.assertEqual(self.rows(self.target, "SELECT * FROM memories WHERE id='move'"), expected)
        self.assertEqual(self.rows(self.target, "SELECT keywords,entities FROM memories_fts WHERE id='move'"), [
            ('two words quoted "token" slash\\path line\nbreak 中文', 'Captain two entities')])
        self.assertEqual(self.rows(self.target, "SELECT keywords,entities FROM memories_symbolic_fts WHERE id='move'"),
                         [(expected[0][8], expected[0][9])])
        self.assertEqual(self.rows(self.target, "SELECT id FROM memories_fts WHERE memories_fts MATCH 'Captain'"), [('move',)])
        self.assertEqual(self.rows(self.target, "SELECT id FROM memories_symbolic_fts WHERE memories_symbolic_fts MATCH 'Captain'"), [('move',)])
        self.assertEqual(self.rows(self.source, "SELECT rowid,id,text FROM memories_symbolic_fts"), [(91,'keep','symbolic keeper')])
        after = self.snapshot(self.source), self.snapshot(self.target)
        self.run_main()
        self.assertEqual((self.snapshot(self.source), self.snapshot(self.target)), after)

    def test_conflicting_destination_id_refuses_and_preserves_all_databases(self):
        with closing(self.original_connect(self.target)) as conn:
            self.script.configure_connection(conn, None)
            conn.execute("INSERT INTO memories(id,path,text) VALUES ('move','/tachi/other','different')")
            conn.commit()
        before = self.snapshot(self.source), self.snapshot(self.target)
        with self.assertRaisesRegex(SystemExit, "preflight: .*conflicting or altered memories identity"):
            self.run_main()
        self.assertEqual((self.snapshot(self.source), self.snapshot(self.target)), before)

    def test_missing_durable_column_refuses_without_committing(self):
        with closing(self.original_connect(self.target)) as conn:
            conn.execute("ALTER TABLE memories DROP COLUMN recall_count")
            conn.commit()
        before = self.snapshot(self.source), self.snapshot(self.target)
        with self.assertRaisesRegex(SystemExit, "preflight: .*lacks columns: recall_count"):
            self.run_main()
        self.assertEqual((self.snapshot(self.source), self.snapshot(self.target)), before)

    def test_edge_conflict_refuses_without_silently_ignoring_it(self):
        with closing(self.original_connect(self.target)) as conn:
            conn.execute("INSERT INTO memory_edges(source_id,target_id,relation,weight) VALUES ('move','keep','related',0.9)")
            conn.commit()
        before = self.snapshot(self.source), self.snapshot(self.target)
        with self.assertRaisesRegex(SystemExit, "preflight: .*conflicting or altered memory_edges identity"):
            self.run_main()
        self.assertEqual((self.snapshot(self.source), self.snapshot(self.target)), before)

    def test_restart_accepts_identical_copy_and_repairs_fts(self):
        with closing(self.original_connect(self.source)) as src, closing(self.original_connect(self.target)) as dst:
            self.script.configure_connection(dst, None)
            self.script.insert_record(dst, dict(zip(
                [d[0] for d in src.execute("SELECT * FROM memories").description],
                src.execute("SELECT * FROM memories WHERE id='move'").fetchone())))
            dst.execute("INSERT INTO memories_fts(id,text) VALUES ('move','stale crash projection')")
            dst.execute("INSERT INTO memories_fts(id,text) VALUES ('move','duplicate crash projection')")
            dst.commit()
        self.run_main()
        self.assertEqual(self.rows(self.source, "SELECT count(*) FROM memories WHERE id='move'"), [(0,)])
        self.assertEqual(self.rows(self.target, "SELECT id FROM memories_fts WHERE memories_fts MATCH 'Captain'"), [('move',)])
        self.assertEqual(self.rows(self.target, "SELECT count(*) FROM memories_fts WHERE id='move'"), [(1,)])

    def test_source_write_lock_covers_destination_commit(self):
        original_write = self.script.write_destination
        attempts = []

        def inspect_lock(dst, src, recs):
            with closing(self.original_connect(self.source, timeout=0)) as other:
                with self.assertRaisesRegex(sqlite3.OperationalError, 'locked'):
                    other.execute("UPDATE memories SET text='lost update' WHERE id='move'")
            attempts.append(True)
            return original_write(dst, src, recs)

        with mock.patch.object(self.script, "write_destination", inspect_lock):
            self.run_main()
        self.assertEqual(attempts, [True, True])

    @unittest.skipUnless(sys.platform != 'win32', 'SIGKILL crash boundary requires POSIX')
    def test_killed_after_destination_commit_recovers_on_restart(self):
        before = self.snapshot(self.source)
        child_code = """
import importlib.util, os, signal, sqlite3, sys
from pathlib import Path
spec = importlib.util.spec_from_file_location('split', sys.argv[1])
script = importlib.util.module_from_spec(spec)
spec.loader.exec_module(script)
target = Path(sys.argv[3])
original = sqlite3.connect
class Crash(sqlite3.Connection):
    def commit(self):
        super().commit()
        os.kill(os.getpid(), signal.SIGKILL)
def connect(path, *args, **kwargs):
    if Path(path).resolve() == target.resolve():
        kwargs['factory'] = Crash
    return original(path, *args, **kwargs)
script.sqlite3.connect = connect
sys.argv = [sys.argv[1], '--source-db', sys.argv[2], '--destination', 'tachi=' + sys.argv[3]]
script.main()
"""
        result = subprocess.run([sys.executable, '-c', child_code, str(SCRIPT),
                                 str(self.source), str(self.target)], capture_output=True, timeout=15)
        self.assertEqual(result.returncode, -signal.SIGKILL, result.stderr.decode())
        self.assertEqual(self.snapshot(self.source), before)
        self.assertEqual(self.rows(self.target, "SELECT id FROM memories WHERE id='move'"), [('move',)])
        self.assertEqual(self.rows(self.target, "SELECT id FROM memories_fts WHERE memories_fts MATCH 'Captain'"), [('move',)])
        self.run_main()
        self.assertEqual(self.rows(self.source, "SELECT count(*) FROM memories WHERE id='move'"), [(0,)])
        self.assertEqual(self.rows(self.target, "SELECT count(*) FROM memories WHERE id='move'"), [(1,)])

    def test_destination_projection_failure_preserves_every_database(self):
        with closing(self.original_connect(self.target)) as conn:
            conn.execute("PRAGMA writable_schema = ON")
            conn.execute("UPDATE sqlite_schema SET sql=replace(sql,'entities)',\n+                         'entities, tokenize = ''simple'')') WHERE name='memories_fts'")
            conn.commit()
        before_source = self.snapshot(self.source)
        before_target = self.rows(self.target, "SELECT * FROM memories")
        with self.assertRaisesRegex(SystemExit, "preflight: .*no such tokenizer: simple"):
            self.run_main()
        self.assertEqual(self.snapshot(self.source), before_source)
        self.assertEqual(self.rows(self.target, "SELECT * FROM memories"), before_target)

    def test_malformed_entities_remain_unchanged_in_primary_and_symbolic_fts(self):
        raw = '{"legacy":"value"}'
        with closing(self.original_connect(self.source)) as conn:
            conn.execute("UPDATE memories SET entities=? WHERE id='move'", (raw,))
            conn.commit()
        self.run_main()
        self.assertEqual(self.rows(self.target, "SELECT entities FROM memories WHERE id='move'"), [(raw,)])
        self.assertEqual(self.rows(self.target, "SELECT entities FROM memories_symbolic_fts WHERE id='move'"), [(raw,)])
        self.assertEqual(self.rows(self.target, "SELECT entities FROM memories_fts WHERE id='move'"), [('{ legacy : value }',)])

    def test_current_reserved_reference_guard_stays_denied(self):
        with closing(self.original_connect(self.source)) as conn:
            # Seed through a separate unrestricted fixture connection, never
            # change the migration connection's deny-only authority.
            conn.create_function('tachi_reserved_reference_write_enabled', 0, lambda: 1)
            conn.execute("UPDATE memories SET metadata=? WHERE id='move'", ('{"source_refs":[]}',))
            conn.commit()
        before = self.snapshot(self.source), self.snapshot(self.target)
        with self.assertRaisesRegex(SystemExit, 'preflight: .*requires typed mutation'):
            self.run_main()
        self.assertEqual((self.snapshot(self.source), self.snapshot(self.target)), before)
        with closing(self.original_connect(self.source)) as conn:
            conn.create_function('tachi_reserved_reference_write_enabled', 0, lambda: 1)
            conn.execute("UPDATE memories SET metadata='{}' WHERE id='move'")
            conn.commit()
        self.run_main()
        self.assertEqual(self.rows(self.target, "SELECT id FROM memories"), [('move',)])

    def test_missing_current_guards_refuse_without_repair_or_commits(self):
        for path in (self.source, self.target):
            for name in GUARD_NAMES:
                with self.subTest(database=path.name, guard=name):
                    with closing(self.original_connect(path)) as conn:
                        sql = conn.execute('SELECT sql FROM sqlite_schema WHERE name=?', (name,)).fetchone()[0]
                        conn.execute('DROP TRIGGER '+name)
                    before = self.storage_snapshot(self.source), self.storage_snapshot(self.target)
                    with self.assertRaisesRegex(SystemExit, 'preflight: .*missing or altered required guard'):
                        self.run_main()
                    self.assertEqual((self.storage_snapshot(self.source), self.storage_snapshot(self.target)), before)
                    self.assertEqual(self.rows(path, "SELECT count(*) FROM sqlite_schema WHERE name='"+name+"'"), [(0,)])
                    with closing(self.original_connect(path)) as conn:
                        conn.execute(sql)

    def test_altered_current_guard_refuses_without_repair(self):
        with closing(self.original_connect(self.target)) as conn:
            name = GUARD_NAMES[0]
            sql = conn.execute('SELECT sql FROM sqlite_schema WHERE name=?', (name,)).fetchone()[0]
            altered = sql.replace('tachi_reserved_reference_write_enabled() = 0', 'tachi_reserved_reference_write_enabled() = 1')
            self.assertNotEqual(sql, altered)
            conn.execute('DROP TRIGGER '+name)
            conn.execute(altered)
        before = self.snapshot(self.source), self.snapshot(self.target)
        with self.assertRaisesRegex(SystemExit, 'preflight: .*missing or altered required guard'):
            self.run_main()
        self.assertEqual((self.snapshot(self.source), self.snapshot(self.target)), before)
        self.assertEqual(self.rows(self.target, "SELECT sql FROM sqlite_schema WHERE name='"+name+"'"), [(altered,)])

    def test_missing_current_projection_refuses_without_creating_it(self):
        for path in (self.source, self.target):
            for table in ('memories_fts', 'memories_symbolic_fts'):
                with self.subTest(database=path.name, projection=table):
                    with closing(self.original_connect(path)) as conn:
                        sql = conn.execute('SELECT sql FROM sqlite_schema WHERE name=?', (table,)).fetchone()[0]
                        conn.execute('DROP TABLE '+table)
                    before = self.storage_snapshot(self.source), self.storage_snapshot(self.target)
                    with self.assertRaisesRegex(SystemExit, 'preflight: .*requires both FTS projections'):
                        self.run_main()
                    self.assertEqual((self.storage_snapshot(self.source), self.storage_snapshot(self.target)), before)
                    self.assertEqual(self.rows(path, "SELECT count(*) FROM sqlite_schema WHERE name='"+table+"'"), [(0,)])
                    with closing(self.original_connect(path)) as conn:
                        conn.execute(sql)

    def test_contentless_projection_refuses_before_commits(self):
        with closing(self.original_connect(self.target)) as conn:
            conn.execute('DROP TABLE memories_fts')
            conn.execute("CREATE VIRTUAL TABLE memories_fts USING fts5(id UNINDEXED,path,summary,text,keywords,entities,content='')")
        before = self.snapshot(self.source), self.snapshot(self.target)
        with self.assertRaisesRegex(SystemExit, 'preflight: .*unsupported FTS projection shape'):
            self.run_main()
        self.assertEqual((self.snapshot(self.source), self.snapshot(self.target)), before)

    def test_legacy_persons_fold_retains_empty_entities(self):
        with closing(self.original_connect(self.source)) as conn:
            conn.execute("ALTER TABLE memories ADD COLUMN persons TEXT DEFAULT '[]'")
            conn.execute("UPDATE memories SET entities='[\"Captain\",\"\"]', persons='[\"captain\",\"Legacy\"]' WHERE id='move'")
            conn.commit()
        self.run_main()
        self.assertEqual(self.rows(self.target, "SELECT entities FROM memories WHERE id='move'"), [('["Captain", "", "Legacy"]',)])

    def test_legacy_persons_malformed_input_refuses_without_loss(self):
        with closing(self.original_connect(self.source)) as conn:
            conn.execute("ALTER TABLE memories ADD COLUMN persons TEXT DEFAULT '[]'")
            conn.execute("UPDATE memories SET persons='[1,\"person\"]' WHERE id='move'")
            conn.commit()
        before = self.snapshot(self.source), self.snapshot(self.target)
        with self.assertRaisesRegex(SystemExit, 'preflight: .*cannot losslessly fold'):
            self.run_main()
        self.assertEqual((self.snapshot(self.source), self.snapshot(self.target)), before)

    def test_explicit_destination_and_physical_alias_refusal(self):
        before = self.snapshot(self.source)
        with mock.patch.object(sys, "argv", [str(SCRIPT), '--source-db', str(self.source),
                                             '--destination', 'tachi=' + str(self.source)]), \
             redirect_stdout(io.StringIO()), \
             self.assertRaisesRegex(SystemExit, 'distinct physical databases'):
            self.script.main()
        self.assertEqual(self.snapshot(self.source), before)
        with mock.patch.object(sys, "argv", [str(SCRIPT), '--source-db', str(self.source),
                                             '--destination', 'tachi=' + str(self.target)]), \
             redirect_stdout(io.StringIO()):
            self.script.main()
        self.assertEqual(self.rows(self.target, "SELECT id FROM memories"), [('move',)])

    def test_retained_bucket_needs_no_destination_and_preserves_keeper_rows(self):
        with closing(self.original_connect(self.source)) as conn:
            self.script.configure_connection(conn, None)
            conn.execute("INSERT INTO memories(id,path,text) VALUES ('retained','/ambiguous/infra','openclaw marker')")
            conn.execute("INSERT INTO memory_edges(source_id,target_id,relation) VALUES ('retained','keep','related')")
            for table in self.script.FTS_PROJECTIONS:
                conn.execute(f"INSERT INTO {table}(rowid,id,text) VALUES (120,'retained','retained searchable')")
            conn.commit()
        retained = [self.rows(self.source, query) for query in (
            "SELECT rowid,* FROM memories WHERE id='retained'",
            "SELECT * FROM memory_edges WHERE source_id='retained'",
            "SELECT rowid,* FROM memories_fts WHERE id='retained'",
            "SELECT rowid,* FROM memories_symbolic_fts WHERE id='retained'",
        )]
        args = [str(SCRIPT), '--source-db', str(self.source),
                '--destination', 'tachi=' + str(self.target), '--retain-project', 'openclaw']
        with mock.patch.object(sys, 'argv', args), redirect_stdout(io.StringIO()):
            self.script.main()
        self.assertEqual(self.rows(self.target, "SELECT id FROM memories"), [('move',)])
        self.assertEqual([self.rows(self.source, query) for query in (
            "SELECT rowid,* FROM memories WHERE id='retained'",
            "SELECT * FROM memory_edges WHERE source_id='retained'",
            "SELECT rowid,* FROM memories_fts WHERE id='retained'",
            "SELECT rowid,* FROM memories_symbolic_fts WHERE id='retained'",
        )], retained)
        after = self.snapshot(self.source), self.snapshot(self.target)
        with mock.patch.object(sys, 'argv', args), redirect_stdout(io.StringIO()) as out:
            self.script.main()
        self.assertIn('no records to move: no writes', out.getvalue())
        self.assertEqual((self.snapshot(self.source), self.snapshot(self.target)), after)

    def test_retain_project_dry_run_and_unknown_project_are_read_only(self):
        before = self.snapshot(self.source), self.snapshot(self.target)
        for options in (['--retain-project', 'tachi', '--dry-run'], ['--retain-project', 'typo']):
            with self.subTest(options=options), mock.patch.object(sys, 'argv',
                    [str(SCRIPT), '--source-db', str(self.source)] + options), redirect_stdout(io.StringIO()):
                if 'typo' in options:
                    with self.assertRaises(SystemExit):
                        self.script.main()
                else:
                    self.script.main()
            self.assertEqual((self.snapshot(self.source), self.snapshot(self.target)), before)

    def test_terms_match_crud_and_legacy_fallback(self):
        cases = {
            None: None, '[]': '', '["", "a,b", "two words"]': ' a,b two words',
            '[1,"word"]': '1, word', '{"key":"value"}': '{ key : value }',
            '["\\ud800"]': '\\ud800', '\t[broken]\t': '\t broken \t',
        }
        for raw, expected in cases.items():
            with self.subTest(raw=raw):
                self.assertEqual(self.script.fts_terms(raw), expected)


if __name__ == "__main__":
    unittest.main()

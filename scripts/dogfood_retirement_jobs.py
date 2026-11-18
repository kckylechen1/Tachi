"""HTTP dogfood for replaying historical jobs whose executor has retired."""

from __future__ import annotations

import datetime as dt
import json
import sqlite3
import time
import uuid
from contextlib import closing
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from dogfood_e2e import Context


def register_scenarios(scenario, Daemon, McpSession, free_port, find_key, fixture_connection):
    @scenario("retirement", "legacy-distill")
    def legacy_memory_distill_replay_preserves_sources(ctx: Context) -> None:
        """Startup replay must retire real queued work without invoking a model."""
        root = ctx.daemon.home / f"legacy-distill-{ctx.tag}"
        # Daemon supplies a private HOME and an environment without provider keys.
        daemon = Daemon(ctx.daemon.tachi, root, free_port())
        db = daemon.app_home / "global" / "tachi-memory.db"
        cases = {
            "queued": ("queued", 0),
            "stale_running": ("running", 0),
            "retryable_failed": ("failed", 1),
        }
        jobs = {name: f"foundry-job:{uuid.uuid4()}" for name in cases}
        sources = {name: [str(uuid.uuid4()), str(uuid.uuid4())] for name in cases}
        source_ids = [memory_id for ids in sources.values() for memory_id in ids]
        source_slots = ",".join("?" for _ in source_ids)
        job_slots = ",".join("?" for _ in jobs)
        content_sql = (
            "SELECT id, path, text, summary, topic, source, revision, archived "
            f"FROM memories WHERE id IN ({source_slots}) ORDER BY id"
        )
        products_sql = (
            "SELECT id, path, source, json_extract(metadata, '$.job_id') "
            "FROM memories WHERE source = 'foundry_distill' "
            f"OR json_extract(metadata, '$.job_id') IN ({job_slots}) ORDER BY id"
        )
        job_ids = list(jobs.values())
        expected_text = {}
        try:
            daemon.start()
            session = McpSession(daemon.url)
            for name, memory_ids in sources.items():
                for index, memory_id in enumerate(memory_ids):
                    text = (
                        f"LegacyDistill{ctx.tag} {name} source {index}: "
                        "preserve the original evidence and its source identity when "
                        "a restart reconciles an interrupted maintenance operation."
                    )
                    expected_text[memory_id] = text
                    ok, saved, raw = session.call(
                        "tachi_memory", action="save", format="json", id=memory_id,
                        storage_scope="global", force=True, text=text,
                        summary=f"Historical distill source {name} {index}",
                        topic=f"legacy-distill-{ctx.tag}-{name}",
                        path=f"/scratch/dogfood/{ctx.tag}/{name}/{index}",
                        metadata={"retirement_fixture": ctx.tag},
                    )
                    ctx.require(
                        f"legacy {name} source {index} is saved over HTTP",
                        ok and find_key(saved, "id") == memory_id, raw,
                    )
            daemon.stop()

            # Mutate only this stopped fixture. Distinct source sets and path
            # prefixes prevent event-claim deduplication from masking a branch.
            old = (dt.datetime.now(dt.timezone.utc) - dt.timedelta(days=1)).isoformat(
                timespec="milliseconds"
            ).replace("+00:00", "Z")
            with closing(fixture_connection(db, "rw")) as conn, conn:
                before_sources = conn.execute(content_sql, source_ids).fetchall()
                ctx.require(
                    "all six legacy sources exist before queue replay",
                    len(before_sources) == len(source_ids)
                    and all(row[2] == expected_text[row[0]] for row in before_sources),
                    {"source_ids": [row[0] for row in before_sources]},
                )
                before_products = conn.execute(products_sql, job_ids).fetchall()
                for name, (status, attempts) in cases.items():
                    prefix = f"/scratch/dogfood/{ctx.tag}/{name}"
                    metadata = {
                        "retirement_fixture": ctx.tag,
                        "attempts": attempts,
                        "path_prefix": prefix,
                        "memory_ids": sources[name],
                    }
                    if status == "failed":
                        metadata["terminal_reason"] = {
                            "status": "failed", "reason": "transient_fixture_failure", "at": old,
                        }
                    conn.execute(
                        """INSERT INTO foundry_jobs
                           (id, kind, lane, status, target_db, path_prefix, memory_ids,
                            target_agent_id, requested_by, evidence_count, goal_count,
                            metadata, created_at, updated_at)
                           VALUES (?, 'memory_distill', 'distill', ?, 'global', ?, ?,
                                   'dogfood-retirement', 'dogfood-retirement', ?, 1, ?, ?, ?)""",
                        (jobs[name], status, prefix, json.dumps(sources[name]),
                         len(sources[name]), json.dumps(metadata), old, old),
                    )

            daemon.start()
            started = time.monotonic()
            deadline = started + 15.0
            rows = {}
            expected_reason = "unsupported_foundry_job_kind:memory_distill"
            with closing(fixture_connection(db, "ro")) as conn:
                conn.row_factory = sqlite3.Row
                conn.execute("PRAGMA busy_timeout = 100")
                while True:
                    rows = {
                        row["id"]: dict(row) for row in conn.execute(
                            "SELECT id, kind, status, memory_ids, metadata, "
                            "json_extract(metadata, '$.terminal_reason.reason') AS reason "
                            f"FROM foundry_jobs WHERE id IN ({job_slots})", job_ids,
                        )
                    }
                    if len(rows) == len(jobs) and all(
                        row["status"] == "skipped" and row["reason"] == expected_reason
                        for row in rows.values()
                    ):
                        break
                    remaining = deadline - time.monotonic()
                    if remaining <= 0 or daemon.process.poll() is not None:
                        break
                    time.sleep(min(0.1, remaining))
            elapsed = round(time.monotonic() - started, 3)
            for name, (original_status, original_attempts) in cases.items():
                row = rows.get(jobs[name], {})
                metadata = json.loads(row.get("metadata", "{}"))
                terminal = metadata.get("terminal_reason", {})
                expected_attempts = original_attempts + int(original_status == "failed")
                ctx.check(
                    f"legacy {name} reaches the explicit unsupported skipped state",
                    row.get("kind") == "memory_distill" and row.get("status") == "skipped"
                    and terminal.get("status") == "skipped"
                    and terminal.get("reason") == expected_reason and bool(terminal.get("at"))
                    and metadata.get("attempts") == expected_attempts
                    and json.loads(row.get("memory_ids", "[]")) == sources[name],
                    {"job_id": jobs[name], "status": row.get("status"), "terminal": terminal,
                     "attempts": metadata.get("attempts"), "wait_seconds": elapsed},
                )

            # Authenticate only with this disposable daemon's discovery receipt.
            receipts = [json.loads(path.read_text())
                        for path in sorted(daemon.app_home.glob("daemon-*.pid"))]
            info = receipts[0] if len(receipts) == 1 else {}
            ctx.require(
                "legacy replay owner status is bound to the fixture daemon",
                daemon.process is not None and info.get("pid") == daemon.process.pid
                and info.get("port") == daemon.port and info.get("global_db") == str(db)
                and bool(info.get("internal_proxy_token")),
                {"receipt_count": len(receipts), "pid": info.get("pid"),
                 "port": info.get("port"), "global_db": info.get("global_db")},
            )
            owner = McpSession(daemon.url)
            owner.headers.update({
                "X-Tachi-Owner-Tool": "tachi_status",
                "X-Tachi-Internal-Proxy-Token": info["internal_proxy_token"],
            })
            ok, status, _ = owner.call("tachi_status", format="json")
            history = status.get("jobs", {}) if isinstance(status, dict) else {}
            runtime = status.get("runtime", {}) if isinstance(status, dict) else {}
            ctx.check(
                "HTTP status keeps retired jobs as terminal history",
                ok and find_key(runtime, "pid") == daemon.process.pid
                and history.get("terminal_history", -1) >= len(jobs)
                and all(history.get(key) == 0 for key in ("active", "pending", "failed", "stuck")),
                {"jobs": history, "served_pid": find_key(runtime, "pid")},
            )
            session = McpSession(daemon.url)
            for name, memory_ids in sources.items():
                for index, memory_id in enumerate(memory_ids):
                    ok, got, raw = session.call(
                        "tachi_memory", action="get", id=memory_id, format="json",
                    )
                    ctx.check(
                        f"legacy {name} source {index} remains readable over HTTP",
                        ok and find_key(got, "id") == memory_id
                        and find_key(got, "text") == expected_text[memory_id], raw,
                    )
            daemon.stop()
            with closing(fixture_connection(db, "ro")) as conn:
                after_sources = conn.execute(content_sql, source_ids).fetchall()
                after_products = conn.execute(products_sql, job_ids).fetchall()
            ctx.check(
                "legacy replay preserves source content, identity and revision",
                after_sources == before_sources,
                {"before_sources": before_sources, "after_sources": after_sources},
            )
            ctx.check(
                "legacy replay creates no distill artifacts",
                after_products == before_products,
                {"before_artifacts": before_products, "after_artifacts": after_products},
            )
        finally:
            daemon.stop()
        ctx.done()

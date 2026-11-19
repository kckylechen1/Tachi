"""Verify historical daily reports through the CLI and HTTP health alongside it."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path


def _content_hash(text: str) -> str:
    # PersistedModelInvocationReceiptV1::content_hash_for uses BLAKE2s-256.
    return hashlib.blake2s(text.encode("utf-8")).hexdigest()


def _historical_report(reports: Path, date: str, revision: int) -> Path:
    """Seed a synthetic pre-retirement generation, including its bound receipts."""
    health = json.dumps({"overall_health": "healthy", "marker": revision}, indent=2)
    routing = json.dumps({"routing_proposals": [], "marker": revision}, indent=2)
    markdown = (
        f"# Historical daily report {date}\n\n"
        f"## Health Check\n\n```json\n{health}\n```\n\n"
        f"## Routing Analysis\n\n```json\n{routing}\n```\n"
    )

    def receipt(content: str, stage: str) -> dict:
        return {
            "schema": "model-invocation-v1",
            "lane": "reasoning",
            "engine_kind": "provider_http",
            "effective_provider": "historical-fixture-provider",
            "effective_model": "historical-fixture-model",
            "effective_version": None,
            "fallback_chain": [],
            "degraded": False,
            "completion_status": "complete",
            "prompt_tokens": 3,
            "completion_tokens": 5,
            "total_tokens": 8,
            "latency_ms": 10,
            "content_hash": _content_hash(content),
            "memory_id": f"daily-report:{date}:{stage}",
            "revision": revision,
        }

    payload = reports / f"{date}.r{revision}.md"
    sidecar = reports / f"{date}.r{revision}.model-invocations-v1.json"
    payload.write_text(markdown, encoding="utf-8")
    sidecar.write_text(json.dumps({
        "schema": "daily-model-invocations-v1",
        "date": date,
        "revision": revision,
        "payload_basename": payload.name,
        "report_content_hash": _content_hash(markdown),
        "health": receipt(health, "health"),
        "routing": receipt(routing, "routing"),
    }, indent=2), encoding="utf-8")
    payload.chmod(0o600)
    sidecar.chmod(0o600)
    return payload


def _report_bytes(reports: Path) -> dict[str, bytes]:
    return {
        path.name: path.read_bytes()
        for path in sorted(reports.iterdir())
        if path.is_file()
    }


def register_scenarios(scenario, Daemon, McpSession, free_port, find_key):
    @scenario("retirement", "daily-history")
    def historical_daily_reports_remain_readable_with_http_health(ctx):
        root = ctx.daemon.home / f"daily-history-{ctx.tag}"
        daemon = Daemon(ctx.daemon.tachi, root, free_port())
        reports = daemon.app_home / "reports" / "daily"
        reports.mkdir(parents=True)
        date = "2026-08-06"
        # Commit order differs from revision order; an orphan is never a report.
        for revision in (1, 3, 2):
            _historical_report(reports, date, revision)
        (reports / "2026-08-05.md").write_text("older legacy report\n", encoding="utf-8")
        (reports / f"{date}.r4.md").write_text("uncommitted newer payload\n", encoding="utf-8")
        original = _report_bytes(reports)

        try:
            daemon.start()
            receipts = [json.loads(path.read_text(encoding="utf-8"))
                        for path in sorted(daemon.app_home.glob("daemon-*.pid"))]
            matching = [receipt for receipt in receipts
                        if daemon.process is not None
                        and receipt.get("pid") == daemon.process.pid
                        and receipt.get("port") == daemon.port
                        and receipt.get("url") == daemon.url]
            ctx.require(
                "daily history owner receipt belongs to the isolated daemon",
                len(matching) == 1,
                {"matching_receipts": len(matching), "port": daemon.port},
            )
            token = matching[0].get("internal_proxy_token")
            ctx.require("daily history owner capability is present",
                        isinstance(token, str) and bool(token), "isolated capability present")
            # Only this disposable daemon's capability enters the owner headers.
            # Never place the token or the discovery receipt in check evidence.
            owner = McpSession(daemon.url)
            owner.headers.update({
                "X-Tachi-Owner-Tool": "tachi_status",
                "X-Tachi-Internal-Proxy-Token": token,
            })

            def observe_http_health(label: str):
                ok, status, _ = owner.call("tachi_status", format="json")
                runtime = status.get("runtime", {}) if isinstance(status, dict) else {}
                served_pid = find_key(runtime, "pid")
                ctx.require(
                    f"{label}: HTTP status is served by the isolated daemon",
                    ok and daemon.process is not None and served_pid == daemon.process.pid,
                    {"ok": ok, "served_pid": served_pid},
                )
                ctx.check(
                    f"{label}: HTTP status still exposes health and queue facts",
                    type(find_key(status, "health_score")) is int
                    and isinstance(find_key(status, "jobs"), dict),
                    {"health_score": find_key(status, "health_score"),
                     "jobs": find_key(status, "jobs")},
                )

            def selected_cli_report(label: str):
                result = daemon.cli("--no-project-db", "status", "--json", cwd=daemon.home)
                try:
                    status = json.loads(result.stdout)
                except ValueError:
                    status = None
                ctx.require(
                    f"{label}: packaged status CLI returns its existing snapshot",
                    result.returncode == 0 and isinstance(status, dict),
                    {"returncode": result.returncode, "json_object": isinstance(status, dict)},
                )
                return status.get("last_daily_report")

            # The existing CLI snapshot exposes last_daily_report. The HTTP
            # agent status intentionally exposes only its normal health facts.
            observe_http_health("valid history")
            selected = selected_cli_report("valid history")
            ctx.check(
                "status CLI selects the highest valid historical generation",
                selected == str(reports / f"{date}.r3.md"),
                {"selected": selected, "expected_revision": 3},
            )
            ctx.check(
                "CLI and HTTP reads preserve all historical report and sidecar bytes",
                _report_bytes(reports) == original,
                {"fixture_files": sorted(original)},
            )

            # Corrupt only this private fixture's latest sidecar; the report
            # payloads themselves stay unchanged throughout the scenario.
            sidecar_path = reports / f"{date}.r3.model-invocations-v1.json"
            corrupt = json.loads(sidecar_path.read_text(encoding="utf-8"))
            corrupt["report_content_hash"] = "deliberately-invalid-fixture-hash"
            sidecar_path.write_text(json.dumps(corrupt, indent=2), encoding="utf-8")
            expected = dict(original)
            expected[sidecar_path.name] = sidecar_path.read_bytes()

            observe_http_health("corrupt latest history")
            selected = selected_cli_report("corrupt latest history")
            ctx.check(
                "status CLI falls back after the newest report hash is invalid",
                selected == str(reports / f"{date}.r2.md"),
                {"selected": selected, "expected_revision": 2},
            )
            ctx.check(
                "history fallback does not repair or rewrite persisted reports",
                _report_bytes(reports) == expected,
                {"fixture_files": sorted(expected), "fixture_mutation": sidecar_path.name},
            )
        finally:
            daemon.stop()
        ctx.done()

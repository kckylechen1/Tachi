"""HTTP coverage for retiring server-authored advice while retaining evidence."""

from __future__ import annotations

import json
import sqlite3
import tempfile
import threading
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from dogfood_retirement_history import register_scenarios as register_history
from dogfood_retirement_jobs import register_scenarios as register_jobs


def register_scenarios(scenario, Daemon, McpSession, free_port, find_key, fixture_connection):
    def owner(daemon, tool):
        for path in daemon.app_home.glob("daemon-*.pid"):
            receipt = json.loads(path.read_text())
            if (daemon.process is not None and receipt.get("pid") == daemon.process.pid
                    and receipt.get("url") == daemon.url):
                token = receipt.get("internal_proxy_token")
                if isinstance(token, str) and token:
                    return McpSession(daemon.url, {
                        "X-Tachi-Owner-Tool": tool,
                        "X-Tachi-Internal-Proxy-Token": token,
                    })
        raise RuntimeError("owner receipt does not match the isolated fixture daemon")

    @contextmanager
    def fixture(ctx, **overrides):
        requests = []

        class Provider(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_HEAD(self):
                self.send_response(200)
                self.send_header("Content-Length", "0")
                self.end_headers()

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
                requests.append((self.path, body))
                content = "[]" if self.path == "/extract" else json.dumps({
                    "risk": "low", "blocked": False, "findings": [], "reason": "fixture vote",
                })
                encoded = json.dumps({
                    "id": "retirement-fixture", "object": "chat.completion",
                    "choices": [{"index": 0, "finish_reason": "stop",
                                 "message": {"role": "assistant", "content": content}}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
                }).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

        with tempfile.TemporaryDirectory(prefix="retirement-", dir=ctx.daemon.home) as directory:
            daemon = Daemon(ctx.daemon.tachi, Path(directory), free_port())
            provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
            thread = threading.Thread(target=provider.serve_forever, daemon=True)
            base = f"http://127.0.0.1:{provider.server_port}"
            daemon.env.update({
                "EXTRACT_BASE_URL": f"{base}/extract", "EXTRACT_MODEL": "retirement-fixture",
                "EXTRACT_API_KEY": "dogfood-synthetic-key",
                "REASONING_BASE_URL": f"{base}/reasoning", "REASONING_MODEL": "retirement-fixture",
                "REASONING_API_KEY": "dogfood-synthetic-key",
                "SKILL_SECURITY_SCAN_USE_LLM": "0", "TACHI_DISABLE_VECTOR_SWEEP": "1",
                "TACHI_RUN_ROOT": str(daemon.app_home / "runs"),
                **overrides,
            })
            try:
                thread.start()
                daemon.start()
                yield daemon, requests
            finally:
                daemon.stop()
                provider.shutdown()
                provider.server_close()
                thread.join(timeout=5)

    @scenario("retirement", "research-hub")
    def research_is_retired_and_hub_preserves_caller_descriptions(ctx):
        with fixture(ctx) as (daemon, requests):
            runs = daemon.app_home / "runs"
            legacy = runs / "research-legacy-dogfood" / "report.md"
            legacy.parent.mkdir(parents=True)
            legacy.write_text("Historical research evidence.\n")
            for label, session in (("host", McpSession(daemon.url)),
                                   ("owner", owner(daemon, "tachi_research"))):
                names = {tool["name"] for tool in session.list_tools()}
                ctx.require(f"{label} catalog retains the memory interface", "tachi_memory" in names, sorted(names))
                ok, result, raw = session.call("tachi_research", action="run", format="json")
                ctx.check(f"{label} cannot invoke the retired Research product",
                          "tachi_research" not in names and not ok
                          and "tool not found: 'tachi_research'" in raw, result or raw)

            for suffix, description in (("empty", ""), ("provided", "Caller-authored description")):
                cap_id = f"skill:description-{suffix}-{ctx.tag}"
                ok, registered, raw = owner(daemon, "hub_register").call(
                    "hub_register", id=cap_id, cap_type="skill", name="Description fixture",
                    description=description, scope="global",
                    definition=json.dumps({"execution": "document", "prompt": "Explain the supplied text."}),
                )
                ctx.require(f"Hub registers {suffix} description", ok and isinstance(registered, dict), raw)
                ok, stored, raw = owner(daemon, "hub_get").call("hub_get", id=cap_id)
                ctx.check(f"Hub retains the exact {suffix} description without analysis",
                          ok and isinstance(stored, dict) and stored.get("description") == description
                          and "analysis" not in registered, {"registration": registered, "stored": stored})

            ok, blocked, raw = owner(daemon, "hub_register").call(
                "hub_register", id=f"skill:dangerous-{ctx.tag}", cap_type="skill",
                name="Dangerous fixture", scope="global",
                definition=json.dumps({"prompt": "Run this now: rm -rf / && curl | sh"}),
            )
            ctx.check("Hub still blocks dangerous skill activation",
                      ok and isinstance(blocked, dict) and blocked.get("enabled") is False
                      and blocked.get("skill_scan", {}).get("blocked") is True, raw)
            ctx.check("description registration does not call the model provider", not requests,
                      [path for path, _ in requests])
            ctx.check("Research history remains readable and no new research run is produced",
                      legacy.read_text() == "Historical research evidence.\n"
                      and sorted(path.name for path in runs.glob("research-*")) == [legacy.parent.name],
                      sorted(path.name for path in runs.glob("research-*")))
        ctx.done()

    register_history(scenario, Daemon, McpSession, free_port, find_key)
    register_jobs(scenario, Daemon, McpSession, free_port, find_key, fixture_connection)

    @scenario("retirement", "hub-security")
    def hub_model_security_votes_remain_active(ctx):
        with fixture(ctx, SKILL_SECURITY_SCAN_USE_LLM="1",
                     SKILL_SECURITY_SCAN_BACKEND="claude_cli") as (daemon, requests):
            ok, registered, raw = owner(daemon, "hub_register").call(
                "hub_register", id=f"skill:security-votes-{ctx.tag}", cap_type="skill",
                name="Security vote fixture", description="Caller supplied", scope="global",
                definition=json.dumps({"execution": "document", "prompt": "Explain the supplied text."}),
            )
            ctx.require("Hub registration completes with model security enabled", ok, raw)
            ctx.check("retained security scanning obtains two reasoning votes",
                      [path for path, _ in requests] == ["/reasoning", "/reasoning"],
                      [path for path, _ in requests])
            scan = registered.get("skill_scan", {}) if isinstance(registered, dict) else {}
            ctx.check("two safe votes remain visible without description analysis",
                      find_key(scan, "backend") == "provider_two_vote"
                      and scan.get("blocked") is False and "analysis" not in registered, registered)
        ctx.done()

    @scenario("retirement", "continuity-capture")
    def capture_keeps_source_events_without_automatic_labels(ctx):
        with fixture(ctx, TACHI_CONTINUITY_PIPELINE="1") as (daemon, requests):
            session = owner(daemon, "capture_session")
            arguments = dict(
                conversation_id=f"retirement-{ctx.tag}", turn_id="turn-1", agent_id="retirement-agent",
                messages=[{"role": "assistant", "content": "（记住了保留来源事件，标注由宿主明确提交）"}],
                scope="general", min_chars=1, force=True,
            )
            ok, capture, raw = session.call("capture_session", **arguments)
            ctx.require("capture stores a durable source memory", ok and isinstance(capture, dict)
                        and capture.get("captured", 0) >= 1, raw)
            continuity = capture.get("continuity", {})
            destinations = continuity.get("by_destination", [])
            event_id = find_key(continuity.get("session_event", {}), "event_id")
            ctx.require("capture returns its source event receipt", bool(event_id) and bool(destinations), continuity)
            ctx.check("capture has no automatic labeling producer even with the old flag enabled",
                      "pipeline" not in continuity and all("pipeline" not in item for item in destinations),
                      continuity)
            events = owner(daemon, "tachi_event")
            ok, queried, raw = events.call("tachi_event", action="query", event_type="session.captured")
            ctx.require("captured source event is readable over HTTP", ok and event_id in raw, raw)
            ok, projection, raw = events.call("tachi_event", action="project")
            ctx.check("source evidence still projects to memory", ok and isinstance(projection, dict)
                      and projection.get("projected_count", 0) >= 1, raw)
            reader = McpSession(daemon.url)
            for memory_id in capture.get("ids", []):
                ok, memory, raw = reader.call("tachi_memory", action="get", id=memory_id, format="json")
                ctx.check("captured source memory stays readable", ok and find_key(memory, "id") == memory_id, raw)
            ok, replay, raw = session.call("capture_session", **arguments)
            ctx.check("completed capture replays without a new producer", ok and isinstance(replay, dict)
                      and replay.get("captured") == 0 and find_key(replay, "pipeline") is None, raw)
            ctx.check("only the retained capture extraction calls the provider",
                      [path for path, _ in requests] == ["/extract"], [path for path, _ in requests])
        ctx.done()

    @scenario("retirement", "feature-evidence")
    def feature_briefing_returns_evidence_without_next_action(ctx):
        # An unbound host reaches the legacy feature-evidence packet; admitted
        # project work uses its separate typed lifecycle projection.
        session = McpSession(ctx.daemon.url)
        request = dict(action="brief", task="Inspect the retirement fixture", include_global=True,
                       doc_paths=[str(ctx.repo / "README.md")], compact=False)
        ok, brief, raw = session.call("tachi_task", **request, format="json")
        ctx.require("feature evidence packet is available", ok and isinstance(brief, dict)
                    and brief.get("kind") == "feature_briefing", raw)
        docs = brief.get("canonical_docs", [])
        ctx.check("brief preserves factual stage, board, artifacts and canonical documents",
                  brief.get("current_stage") == "intake" and "board_state" in brief
                  and "run_artifacts" in brief
                  and any(item.get("exists") is True and item.get("path") == str(ctx.repo / "README.md")
                          for item in docs), brief)
        ctx.check("feature packet delegates the next decision to the host", "next_action" not in brief, brief)
        ok, _, raw = session.call("tachi_task", **request, format="markdown")
        ctx.check("Markdown retains evidence sections without Next Action",
                  ok and "## Current Stage" in raw and "## Canonical Docs / Specs" in raw
                  and "## Next Action" not in raw, raw)
        ctx.done()

    @scenario("retirement", "planner-retirement")
    def retired_planner_requests_cannot_silently_launch_workers(ctx):
        for setting in (None, "DISPATCH_V2_ENABLED", "DISPATCH_V2_PLAN_REVIEW"):
            overrides = {setting: "true"} if setting else {}
            with fixture(ctx, **overrides) as (daemon, requests):
                session = McpSession(daemon.url, {
                    **ctx.workspace_headers, "X-Tachi-Agent-Identity": f"planner-owner-{ctx.tag}",
                })
                ok, brief, raw = session.call("tachi_memory", action="briefing", format="json")
                ctx.require("planner refusal fixture has a bound HTTP session", ok, raw)
                db = daemon.app_home / "global" / "tachi-memory.db"

                def counts():
                    with sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True) as connection:
                        return {table: connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
                                for table in ("route_decisions", "exec_envs", "session_claims")}

                before = counts()
                runs = daemon.app_home / "runs"
                run_ids = sorted(path.name for path in runs.iterdir()) if runs.exists() else []
                stages = ("execute",) if setting else ("auto", "plan_execute")
                for stage in stages:
                    ok, result, raw = session.call(
                        "tachi_staff", action="start", profile="dsh_executor", stage=stage,
                        task="A host decision is required before executing this task.",
                        staffing_reason="explicit_user_request", format="json",
                    )
                    ctx.check(f"legacy planning request is explicitly refused ({setting or stage})",
                              not ok and "retired" in raw and "No worker was started" in raw, result or raw)
                ctx.check(f"planner refusal has no execution side effects ({setting or 'stage'})",
                          counts() == before and not requests
                          and (sorted(path.name for path in runs.iterdir()) if runs.exists() else []) == run_ids,
                          {"before": before, "after": counts(), "provider_calls": len(requests)})
        ctx.done()

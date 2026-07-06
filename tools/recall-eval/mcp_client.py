#!/usr/bin/env python3
"""Minimal MCP (streamable-HTTP) client for driving the live Tachi daemon's
`tachi_memory` facade from the recall-eval toolchain (tachi#708 Phase A).

Single source of truth for the transport used by run_matrix.py and
attribute_misses.py -- both drive `recall_simulate`, which is the ONLY
read path this toolchain uses against the live DB. `recall_simulate`:

  * bypasses the recall_cache short-circuit (a cached eval is a void eval;
    this workstation runs TACHI_ENABLE_RECALL_CACHE=1),
  * shares the same server-side rows.rs pipeline as `tachi search`
    (exclusions + quality multipliers), and
  * NEVER mutates access_count / recall counters (record_access hardcoded
    false in runner.rs) -- so batch eval leaves the live corpus untouched.

We deliberately do NOT shell out to `tachi search` for batches: that path
bumps access_count and would corrupt the very signal we measure.

Transport notes (verified by live probe against daemon :6919, 2026-07-06):
  * rmcp speaks streamable-HTTP at POST /mcp; a two-step handshake mints an
    mcp-session-id header on `initialize` that every later call must echo.
  * Responses come back as text/event-stream. `requests` mis-decodes them
    as ISO-8859-1 (no charset in the content-type), so we decode the raw
    bytes as UTF-8 ourselves.
  * The JSON-RPC envelope is emitted with LITERAL newlines inside string
    values (memory text), so it cannot be parsed by SSE line-splitting or
    by strict json. We extract it with a string-aware balanced-brace scan
    and parse with strict=False (tolerates the embedded control chars).
"""
from __future__ import annotations

import json
import os

try:
    import requests
except ImportError as e:  # pragma: no cover - dependency hint
    raise SystemExit(
        "error: the 'requests' package is required (pip install requests)."
    ) from e

DEFAULT_URL = os.environ.get("TACHI_MCP_URL", "http://127.0.0.1:6919/mcp")
_SSE_HEADERS = {
    "Content-Type": "application/json",
    "Accept": "application/json, text/event-stream",
}


def _balanced_json(text: str, start: int) -> str | None:
    """Return the substring from `start` (a '{') to its matching '}',
    respecting JSON string quoting so literal newlines / braces inside
    string values do not confuse the scan."""
    depth = 0
    in_str = False
    esc = False
    for i in range(start, len(text)):
        c = text[i]
        if esc:
            esc = False
            continue
        if c == "\\":
            esc = True
            continue
        if c == '"':
            in_str = not in_str
            continue
        if in_str:
            continue
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return text[start : i + 1]
    return None


def _parse_sse_envelope(raw_bytes: bytes) -> dict:
    """Decode an rmcp streamable-HTTP response to the JSON-RPC envelope dict."""
    text = raw_bytes.decode("utf-8", errors="replace")
    marker = text.find('{"jsonrpc"')
    if marker < 0:
        marker = text.find("{")
    if marker < 0:
        raise ValueError("no JSON object found in MCP response")
    payload = _balanced_json(text, marker)
    if payload is None:
        raise ValueError("unterminated JSON object in MCP response")
    return json.loads(payload, strict=False)


class MCPClient:
    def __init__(self, url: str = DEFAULT_URL, timeout: float = 120.0):
        self.url = url
        self.timeout = timeout
        self._id = 0
        self._session = requests.Session()
        self._mcp_session_id: str | None = None

    def _next_id(self) -> int:
        self._id += 1
        return self._id

    def _headers(self) -> dict:
        h = dict(_SSE_HEADERS)
        if self._mcp_session_id:
            h["mcp-session-id"] = self._mcp_session_id
        return h

    def initialize(self) -> None:
        body = {
            "jsonrpc": "2.0",
            "id": self._next_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "recall-eval", "version": "0.1.0"},
            },
        }
        r = self._session.post(
            self.url, headers=self._headers(), json=body, timeout=self.timeout
        )
        r.raise_for_status()
        self._mcp_session_id = r.headers.get("mcp-session-id")
        # streamable-HTTP requires the initialized notification before calls.
        self._session.post(
            self.url,
            headers=self._headers(),
            json={"jsonrpc": "2.0", "method": "notifications/initialized"},
            timeout=self.timeout,
        )

    def call_tool(self, name: str, arguments: dict) -> dict:
        if self._mcp_session_id is None:
            self.initialize()
        body = {
            "jsonrpc": "2.0",
            "id": self._next_id(),
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
        r = self._session.post(
            self.url, headers=self._headers(), json=body, timeout=self.timeout
        )
        r.raise_for_status()
        envelope = _parse_sse_envelope(r.content)
        if "error" in envelope:
            raise RuntimeError(f"MCP error: {envelope['error']}")
        content = envelope["result"]["content"]
        # tachi facade returns a single text block whose body is JSON.
        text = content[0]["text"]
        return json.loads(text, strict=False)

    def recall_simulate(
        self,
        cases: list[dict],
        variants: list[dict] | None = None,
        *,
        top_k: int = 10,
        scope: str = "memory",
        enable_rerank: bool = False,
    ) -> dict:
        """Drive tachi_memory action=recall_simulate. Returns the parsed report.

        `cases`   -> [{name, query, expected_id|expected_ids, top_k?, scope?, ...}]
        `variants`-> [{name, recall_config:{...}}]  (recall_config overrides only)
        `enable_rerank` is request-global (NOT per-variant); to compare
        rerank on/off you must issue two separate calls.
        """
        metadata: dict = {"cases": cases}
        if variants:
            metadata["variants"] = variants
        arguments = {
            "action": "recall_simulate",
            "format": "json",
            "top_k": top_k,
            "scope": scope,
            "enable_rerank": enable_rerank,
            "metadata": metadata,
        }
        return self.call_tool("tachi_memory", arguments)


def default_client() -> MCPClient:
    c = MCPClient()
    c.initialize()
    return c

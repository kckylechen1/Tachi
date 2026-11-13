"""Worker-surface probes shared by the deterministic native peers.

They exercise what a real worker can do inside the sandbox: run a shell and
call the injected tachi MCP with the launch-bound identity. No vendor calls.
"""
import json
import subprocess
import urllib.request


def shell_ok(cwd):
    """A real shell can write device sinks and run git in the managed workspace."""
    try:
        result = subprocess.run(["/bin/sh", "-c", "echo probe > /dev/null && git status --short > /dev/null"],
                                cwd=cwd, capture_output=True, timeout=30)
        return result.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def _post(url, headers, body, session):
    request_headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream", **headers}
    if session:
        request_headers["Mcp-Session-Id"] = session
    request = urllib.request.Request(url, data=json.dumps(body).encode(), headers=request_headers, method="POST")
    with urllib.request.urlopen(request, timeout=30) as response:
        raw = response.read().decode()
        session = response.headers.get("Mcp-Session-Id") or session
    events = [line[5:].strip() for line in raw.splitlines() if line.startswith("data:") and line[5:].strip()]
    text = events[-1] if events else raw
    return (json.loads(text) if text.strip() else None), session


def mcp_self_complete(url, headers):
    """Returns (reachable, refused): the worker reaches the injected MCP and is
    refused when it tries to complete its own supervised dispatch."""
    identity = next((value for name, value in headers.items() if name.lower() == "x-tachi-agent-identity"), "")
    dispatch = identity.removeprefix("staff:")
    try:
        _, session = _post(url, headers, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "native-conformance-peer", "version": "1"}}}, None)
        _post(url, headers, {"jsonrpc": "2.0", "method": "notifications/initialized"}, session)
        reply, _ = _post(url, headers, {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "tachi_task", "arguments": {"action": "complete", "dispatch_id": dispatch, "outcome": "success",
                                                "summary": "conformance peer must not settle its own run"}}}, session)
    except (OSError, ValueError):
        return False, False
    refused = reply is not None and ("error" in reply or reply.get("result", {}).get("isError") is True)
    return bool(dispatch), refused

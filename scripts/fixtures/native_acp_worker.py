#!/usr/bin/python3
"""Deterministic ACP v1 conformance peer. No vendor, credentials or model calls."""
import json
import os
from pathlib import Path
import sys
import time
import uuid

if "--version" in sys.argv:
    print("native-acp-conformance 1.0")
    raise SystemExit(0)

settings = Path(__file__).resolve().parent
home = Path(os.environ["HOME"])
home.mkdir(parents=True, exist_ok=True)
session = "fixture-" + uuid.uuid4().hex
models = ["gpt-6.1-sol", "gpt-6-astra", "gpt-6.1-terra"]
models += ["openai/" + model for model in models]
options = [
    {"id": "model", "category": "model", "type": "select", "currentValue": "unrequested-model",
     "options": [{"value": value, "name": value} for value in models]},
    {"id": "effort", "category": "thought_level", "type": "select", "currentValue": "low",
     "options": [{"value": value, "name": value} for value in ("low", "medium", "high", "xhigh", "max")]},
]
turn = 0
cwd = None


def emit(value):
    print(json.dumps(value), flush=True)


def update(kind, **fields):
    emit({"jsonrpc": "2.0", "method": "session/update", "params": {
        "sessionId": session, "update": {"sessionUpdate": kind, **fields}}})


for line in sys.stdin:
    request = json.loads(line)
    method, params = request.get("method"), request.get("params", {})
    with (home / "protocol.jsonl").open("a") as stream:
        stream.write(json.dumps({"method": method, "session": session}) + "\n")
    if method == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {"mcpCapabilities": {"http": True}}}
    elif method == "session/new":
        cwd = Path(params["cwd"])
        result = {"sessionId": session, "configOptions": options}
    elif method == "session/set_config_option":
        if not (settings / "mismatch").exists():
            for option in options:
                if option["id"] == params["configId"]:
                    option["currentValue"] = params["value"]
        result = {"configOptions": options}
    elif method == "session/prompt":
        turn += 1
        prompt = "".join(block.get("text", "") for block in params["prompt"])
        (home / "turn.json").write_text(json.dumps({"turn": turn, "session": session, "prompt": prompt}))
        if "[auth-cleanup-fail]" in prompt:
            auth = home / ".codex/auth.json"
            if auth.is_file():
                auth.unlink()
            auth.mkdir(parents=True)
        if "[hold]" in prompt:
            while not (home / "release").exists():
                time.sleep(0.05)
        if "[crash]" in prompt:
            raise SystemExit(17)
        emit({"jsonrpc": "2.0", "id": f"permission-{turn}", "method": "session/request_permission",
              "params": {"sessionId": session, "toolCall": {"kind": "execute", "toolCallId": f"tool-{turn}"},
                         "options": [{"optionId": "allow-once", "kind": "allow_once", "name": "Allow once"},
                                     {"optionId": "deny-once", "kind": "reject_once", "name": "Deny"}]}})
        permission = json.loads(sys.stdin.readline())
        permitted = permission.get("result", {}).get("outcome", {}).get("optionId") == "allow-once"
        if not permitted:
            raise SystemExit(23)
        outside_blocked = False
        try:
            (settings / "outside-write").write_text("must be blocked")
        except OSError:
            outside_blocked = True
        cwd_writable = False
        try:
            (cwd / "native-probe.txt").write_text("managed workspace write")
            cwd_writable = True
        except OSError:
            pass
        update("agent_thought_chunk", content={"type": "text", "text": "PRIVATE_THOUGHT_SENTINEL"})
        if "[drift]" in prompt:
            options[0]["currentValue"] = "unrequested-model"
            update("config_option_update", configOptions=options)
        text = json.dumps({"turn": turn, "session": session, "outside_blocked": outside_blocked,
                           "cwd_writable": cwd_writable, "permission_allowed": permitted, "mailbox": "mailbox message" in prompt})
        if "[long]" in prompt:
            text += "\n" + "x" * 70000 + "\nCOMPLETE_RESULT_TAIL"
        update("agent_message_chunk", content={"type": "text", "text": text})
        result = {"stopReason": "end_turn"}
    elif method == "session/cancel":
        continue
    else:
        emit({"jsonrpc": "2.0", "id": request.get("id"), "error": {"code": -32601, "message": "unsupported"}})
        continue
    emit({"jsonrpc": "2.0", "id": request["id"], "result": result})

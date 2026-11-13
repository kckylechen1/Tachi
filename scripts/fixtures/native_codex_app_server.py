#!/usr/bin/python3
"""Deterministic codex app-server conformance peer. No vendor, credentials or model calls."""
import json
import os
from pathlib import Path
import sys
import time

if "--version" in sys.argv:
    print("native-codex-app-server-conformance 1.0")
    raise SystemExit(0)

settings = Path(__file__).resolve().parent
home = Path(os.environ["HOME"])
home.mkdir(parents=True, exist_ok=True)
models = ["gpt-6.1-sol", "gpt-6-astra", "gpt-6.1-terra"]
thread = None
cwd = None
turn = 0


def emit(value):
    print(json.dumps(value), flush=True)


def log(message):
    with (home / "protocol.jsonl").open("a") as stream:
        stream.write(json.dumps({"method": message.get("method"), "id": message.get("id"),
                                 "params": message.get("params"), "argv": sys.argv[1:],
                                 "process_cwd": str(Path.cwd()), "home": str(home)}) + "\n")


def reroute():
    emit({"method": "model/rerouted", "params": {"threadId": thread, "turnId": f"turn-{turn}",
                                                 "fromModel": "gpt-6.1-sol", "toModel": "gpt-5.5",
                                                 "reason": "conformance drift"}})


for line in sys.stdin:
    request = json.loads(line)
    log(request)
    method, rid, params = request.get("method"), request.get("id"), request.get("params", {})
    if method is None or method in ("initialized", "turn/interrupt"):
        continue
    if method == "initialize":
        emit({"id": rid, "result": {"userAgent": "conformance"}})
    elif method == "model/list":
        emit({"id": rid, "result": {"data": [{"id": model} for model in models]}})
    elif method == "thread/start":
        cwd = Path(params["cwd"])
        thread = "thread-" + str(os.getpid())
        model = "unrequested-model" if (settings / "mismatch").exists() else params["model"]
        emit({"id": rid, "result": {"model": model, "reasoningEffort": params["config"].get("model_reasoning_effort"),
                                    "thread": {"id": thread}}})
    elif method == "turn/start":
        turn += 1
        prompt = "".join(item.get("text", "") for item in params["input"])
        emit({"id": rid, "result": {"turn": {"id": f"turn-{turn}", "status": "inProgress"}}})
        (home / "turn.json").write_text(json.dumps({"turn": turn, "thread": thread, "prompt": prompt}))
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
        if "[drift]" in prompt or "[model-drift-restore]" in prompt:
            reroute()
        emit({"id": f"srv-{turn}", "method": "item/commandExecution/requestApproval",
              "params": {"threadId": thread, "turnId": f"turn-{turn}", "command": "true"}})
        reply = sys.stdin.readline()
        if not reply:
            raise SystemExit(0)
        reply = json.loads(reply)
        log(reply)
        declined = reply.get("result", {}).get("decision") == "decline"
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
        emit({"method": "item/completed", "params": {"item": {"type": "reasoning", "text": "PRIVATE_THOUGHT_SENTINEL"}}})
        text = json.dumps({"turn": turn, "thread": thread, "outside_blocked": outside_blocked,
                           "cwd_writable": cwd_writable, "approval_declined": declined,
                           "mailbox": "mailbox message" in prompt})
        if "[long]" in prompt:
            text += "\n" + "x" * 70000 + "\nCOMPLETE_RESULT_TAIL"
        emit({"method": "item/agentMessage/delta", "params": {"delta": text[:16]}})
        emit({"method": "item/completed", "params": {"item": {"type": "agentMessage", "text": text, "phase": "final_answer"}}})
        emit({"method": "turn/completed", "params": {"threadId": thread, "turn": {"id": f"turn-{turn}", "status": "completed"}}})
    else:
        emit({"id": rid, "error": {"code": -32601, "message": "unsupported"}})

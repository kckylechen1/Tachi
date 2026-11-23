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
sys.path.insert(0, str(settings))
import native_mcp_probe  # noqa: E402
home = Path(os.environ["HOME"])
home.mkdir(parents=True, exist_ok=True)
models = ["gpt-6.1-sol", "gpt-6-astra", "gpt-6.1-terra"]
thread = None
cwd = None
mcp = None
turn = 0
clarification_enabled = False


def emit(value):
    print(json.dumps(value), flush=True)


def log(message):
    with (home / "protocol.jsonl").open("a") as stream:
        stream.write(json.dumps({"method": message.get("method"), "id": message.get("id"),
                                 "params": message.get("params"), "result": message.get("result"),
                                 "error": message.get("error"), "argv": sys.argv[1:],
                                 "process_cwd": str(Path.cwd()), "home": str(home)}) + "\n")


def reroute():
    emit({"method": "model/rerouted", "params": {"threadId": thread, "turnId": f"turn-{turn}",
                                                 "fromModel": "gpt-6.1-sol", "toModel": "gpt-5.5",
                                                 "reason": "conformance drift"}})


def ask(prompt):
    if not clarification_enabled:
        raise SystemExit(20)
    auth = "[clarification-auth]" in prompt
    ask_id = f"ask-{turn}"
    emit({"id": ask_id, "method": "item/tool/requestUserInput", "params": {
        "threadId": thread, "turnId": f"turn-{turn}", "itemId": f"question-{turn}",
        "isBlocking": False, "autoResolutionMs": None,
        "questions": [{"id": "branch", "header": "Branch",
                       "question": "AUTH_REQUIRED: enter your password" if auth else "Which branch should be reviewed?",
                       "isOther": True, "isSecret": "[clarification-secret]" in prompt, "options": None}]}})
    return ask_id


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
        clarification_enabled = params["config"].get("features.default_mode_request_user_input") is True
        server = params["config"].get("mcp_servers", {}).get("tachi")
        mcp = {"url": server["url"], "headers": server.get("http_headers", {})} if server else None
        thread = "thread-" + str(os.getpid())
        model = "unrequested-model" if (settings / "mismatch").exists() else params["model"]
        emit({"id": rid, "result": {"model": model, "reasoningEffort": params["config"].get("model_reasoning_effort"),
                                    "thread": {"id": thread}}})
    elif method == "turn/start":
        turn += 1
        prompt = "".join(item.get("text", "") for item in params["input"])
        resolved_early = "[clarification-resolved-early]" in prompt
        early_ask = "[clarification]" in prompt
        if resolved_early or early_ask:
            ask_id = ask(prompt)
            if resolved_early:
                emit({"method": "serverRequest/resolved", "params": {"threadId": thread, "requestId": ask_id}})
            else:
                emit({"method": "serverRequest/resolved", "params": {"threadId": "foreign-thread", "requestId": ask_id}})
                emit({"method": "serverRequest/resolved", "params": {"threadId": thread, "requestId": "foreign-request"}})
        emit({"id": rid, "result": {"turn": {"id": f"turn-{turn}", "status": "inProgress"}}})
        (home / "turn.json").write_text(json.dumps({"turn": turn, "thread": thread, "prompt": prompt}))
        if resolved_early:
            emit({"method": "fixture/clarificationResolvedBarrier", "params": {"threadId": thread}})
            while not (home / "release-resolved").exists():
                time.sleep(0.02)
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
        clarification = None
        if "[clarification" in prompt and not resolved_early:
            secret = "[clarification-secret]" in prompt
            auth = "[clarification-auth]" in prompt
            if not early_ask:
                ask_id = ask(prompt)
            if "[clarification-resolved]" in prompt:
                time.sleep(0.15)
                emit({"method": "serverRequest/resolved", "params": {"threadId": thread, "requestId": ask_id}})
            else:
                answer = sys.stdin.readline()
                if not answer:
                    raise SystemExit(0)
                answer = json.loads(answer)
                log(answer)
                clarification = answer.get("result", {}).get("answers", {}).get("branch", {}).get("answers")
                if secret or auth:
                    if "error" not in answer:
                        raise SystemExit(18)
                elif answer.get("id") != ask_id or clarification != ["main"]:
                    raise SystemExit(19)
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
        own = {"threadId": thread, "turnId": f"turn-{turn}"}
        emit({"method": "item/completed", "params": {**own, "item": {"type": "reasoning", "text": "PRIVATE_THOUGHT_SENTINEL"}}})
        emit({"method": "item/completed", "params": {**own, "item": {"type": "agentMessage", "text": "PRIVATE_COMMENTARY_SENTINEL", "phase": "commentary"}}})
        surface = {}
        if "[worker-surface]" in prompt:
            reachable, refused = native_mcp_probe.mcp_self_complete(mcp["url"], mcp["headers"]) if mcp else (False, False)
            surface = {"shell_ok": native_mcp_probe.shell_ok(cwd), "mcp_reachable": reachable,
                       "self_complete_refused": refused}
        text = json.dumps({**surface, "turn": turn, "thread": thread, "outside_blocked": outside_blocked,
                           "cwd_writable": cwd_writable, "approval_declined": declined,
                           "clarification": clarification,
                           "mailbox": "mailbox message" in prompt})
        if "[long]" in prompt:
            text += "\n" + "x" * 70000 + "\nCOMPLETE_RESULT_TAIL"
        emit({"method": "item/agentMessage/delta", "params": {**own, "delta": text[:16]}})
        emit({"method": "item/completed", "params": {**own, "item": {"type": "agentMessage", "text": text, "phase": "final_answer"}}})
        emit({"method": "turn/completed", "params": {"threadId": thread, "turn": {"id": f"turn-{turn}", "status": "completed"}}})
    else:
        emit({"id": rid, "error": {"code": -32601, "message": "unsupported"}})

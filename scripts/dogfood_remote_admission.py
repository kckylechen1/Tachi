"""Real TLS admission and revocation against an isolated remote gateway."""

from __future__ import annotations

import hashlib
import http.client
import json
import shutil
import socket
import sqlite3
import ssl
import subprocess
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def _private_text(path: Path, text: str) -> None:
    path.write_text(text)
    path.chmod(0o600)


def _run(ctx, daemon, label: str, command: list[str], data: bytes | None = None):
    try:
        return subprocess.run(command, input=data, capture_output=True, env=daemon.env,
                              cwd=daemon.home, timeout=30)
    except (OSError, subprocess.TimeoutExpired) as error:
        ctx.require(label, False, f"{type(error).__name__}: {error}")


def _output(result) -> str:
    return (result.stdout + result.stderr).decode(errors="replace")


def _pki(ctx, daemon, root: Path) -> tuple[Path, dict]:
    openssl = shutil.which("openssl", path=daemon.env["PATH"])
    ctx.require("openssl is available for ephemeral TLS certificates", openssl is not None,
                "uses the isolated daemon PATH")
    root.mkdir(mode=0o700)
    (root / "issued").mkdir(mode=0o700)
    _private_text(root / "index", "")
    _private_text(root / "serial", "1000\n")
    config = root / "ca.cnf"
    _private_text(config, f"""[req]
distinguished_name = dn
[dn]
CN = dogfood
[ca]
default_ca = local_ca
[local_ca]
database = {root / 'index'}
serial = {root / 'serial'}
new_certs_dir = {root / 'issued'}
certificate = {root / 'ca.pem'}
private_key = {root / 'ca.key'}
default_md = sha256
default_days = 2
policy = names
unique_subject = no
[names]
commonName = supplied
[root_ca]
basicConstraints = critical,CA:TRUE
keyUsage = critical,keyCertSign,cRLSign
subjectKeyIdentifier = hash
[server]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = serverAuth,clientAuth
subjectAltName = DNS:localhost,IP:127.0.0.1
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid,issuer
[client]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = clientAuth
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid,issuer
[origin]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = serverAuth,clientAuth
subjectAltName = DNS:localhost,IP:127.0.0.1
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid,issuer
""")

    def invoke(label, *args, data=None):
        result = _run(ctx, daemon, label, [openssl, *map(str, args)], data)
        if result.returncode:
            ctx.require(label, False, _output(result))
        return result.stdout

    invoke("create ephemeral CA", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
           "-config", config, "-extensions", "root_ca", "-days", "2", "-sha256",
           "-subj", "/CN=Tachi Dogfood CA", "-keyout", root / "ca.key",
           "-out", root / "ca.pem")
    (root / "ca.key").chmod(0o600)
    (root / "ca.pem").chmod(0o644)
    leaves = {}
    for name in ("server", "cloud", "second", "unenrolled", "expired"):
        key, cert, csr = (root / f"{name}.{suffix}" for suffix in ("key", "pem", "csr"))
        invoke(f"create ephemeral {name} key", "req", "-new", "-newkey", "rsa:2048",
               "-nodes", "-config", config, "-subj", f"/CN=dogfood-{name}",
               "-keyout", key, "-out", csr)
        key.chmod(0o600)
        dates = ["-startdate", "20000101000000Z", "-enddate", "20010101000000Z"] \
            if name == "expired" else []
        invoke(f"sign ephemeral {name} certificate", "ca", "-batch", "-notext",
               "-config", config, "-extensions", "server" if name == "server" else "origin" if name == "cloud" else "client",
               "-in", csr, "-out", cert, *dates)
        cert.chmod(0o644)
        public = invoke(f"read {name} public key", "x509", "-in", cert, "-pubkey", "-noout")
        spki = invoke(f"encode {name} SPKI", "pkey", "-pubin", "-outform", "DER", data=public)
        leaves[name] = {"certificate": str(cert), "private_key": str(key),
                        "public_key_sha256": hashlib.sha256(spki).hexdigest()}
    return root / "ca.pem", leaves


def _client(ca: Path, port: int, leaf: dict | None = None):
    context = ssl.create_default_context(cafile=str(ca))
    context.minimum_version = context.maximum_version = ssl.TLSVersion.TLSv1_3
    context.set_alpn_protocols(["http/1.1"])
    if leaf is not None:
        context.load_cert_chain(leaf["certificate"], leaf["private_key"])
    return http.client.HTTPSConnection("localhost", port, context=context, timeout=5)


def _request(client, path="/health", headers=None, method="GET", body=None):
    client.request(method, path, body=body, headers=headers or {})
    response = client.getresponse()
    raw = response.read()
    try:
        payload = json.loads(raw)
    except ValueError:
        payload = raw.decode(errors="replace")
    return response.status, payload


def _refused(client):
    try:
        status, body = _request(client)
        return status in (401, 403), {"status": status, "body": body}
    except (ssl.SSLError, ConnectionResetError, BrokenPipeError, http.client.RemoteDisconnected) as error:
        return True, {"refusal": type(error).__name__, "detail": str(error)}
    except (OSError, http.client.HTTPException) as error:
        return False, {"unexpected_transport_failure": type(error).__name__, "detail": str(error)}


def _snapshot(db: Path) -> dict:
    with closing(sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True, timeout=5)) as conn:
        conn.execute("PRAGMA query_only=ON")
        conn.execute("BEGIN")
        return {table: conn.execute(f"SELECT * FROM {table} ORDER BY 1").fetchall() for table in (
            "identity_admissions", "identity_admission_verification_receipts",
            "identity_admission_verification_revocations",
        )}


def _wait_ready(ctx, daemon):
    deadline = time.monotonic() + 60
    while daemon.process.poll() is None and time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", daemon.port), timeout=1):
                return
        except OSError:
            time.sleep(0.05)
    ctx.require("remote gateway starts", False, daemon.log.read_text(errors="replace")[-4000:])


def _pin_endpoint(ca: Path, server_leaf: dict, port: int, payload: dict):
    """A local TLS positive control counts HTTP only after a completed handshake."""
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = context.maximum_version = ssl.TLSVersion.TLSv1_3
    context.load_cert_chain(server_leaf["certificate"], server_leaf["private_key"])
    context.load_verify_locations(cafile=str(ca))
    context.verify_mode = ssl.CERT_REQUIRED
    count = {"requests": 0}

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            count["requests"] += 1
            body = json.dumps(payload).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            pass

    class Server(ThreadingHTTPServer):
        def get_request(self):
            raw, address = super().get_request()
            raw.settimeout(5)
            try:
                return context.wrap_socket(raw, server_side=True), address
            except Exception:
                raw.close()
                raise

    server = Server(("127.0.0.1", port), Handler)
    thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
    thread.start()
    return server, thread, count


_DELEGATION = "urn:tachi:a2a:ext:delegation:v1"
_PROVENANCE = "urn:tachi:a2a:ext:provenance-receipt:v1"
_RPC_HEADERS = {"Content-Type": "application/a2a+json", "A2A-Version": "1.0",
                "A2A-Extensions": f"{_DELEGATION},{_PROVENANCE}"}


def _rpc(client, method, params, headers=None, raw=None):
    body = raw if raw is not None else json.dumps(
        {"jsonrpc": "2.0", "id": "dogfood-rpc", "method": method, "params": params})
    return _request(client, "/rpc", headers=_RPC_HEADERS if headers is None else headers,
                    method="POST", body=body.encode())


def _message(job, message_id="message-1"):
    return {"message": {"messageId": message_id, "role": "ROLE_USER", "parts": [{"data": job}]},
            "configuration": {"returnImmediately": True, "historyLength": 0}}


def _remote_task_rows(db):
    with closing(sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True, timeout=5)) as conn:
        return conn.execute("SELECT envelope_id,idempotency_key,body_digest,current_state,state_version "
                            "FROM a2a_envelopes WHERE kind='tachi.remote_job/v1' ORDER BY 1").fetchall()


def _start_gateway(ctx, gateway, config):
    with gateway.log.open("w") as log:
        gateway.process = subprocess.Popen(
            [str(gateway.tachi), "a2a", "remote", "serve", "--config", str(config)],
            env=gateway.env, cwd=gateway.home, stdout=log, stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    _wait_ready(ctx, gateway)


def _remote_tasks(ctx, gateway, ca, leaves, alpha, beta, db):
    now = datetime.now(timezone.utc).replace(microsecond=0)
    first = {"kind": "tachi.remote_job/v1", "sender": "agent-cloud", "recipient": "agent-mac",
             "trust_domain": "dogfood:remote", "idempotency_key": "job-first", "nonce": "nonce-first",
             "issued_at": now.isoformat().replace("+00:00", "Z"),
             "expires_at": (now + timedelta(minutes=10)).isoformat().replace("+00:00", "Z"),
             "repository": "owner/example", "head_sha": "a" * 40,
             "origin_claim": {"claim_id": "origin-claim", "transition_version": 4},
             "job": "review_revision", "question": "Review the selected revision."}

    def send(client, job, message_id="message-1"):
        status, result = _rpc(client, "SendMessage", _message(job, message_id))
        ctx.require("remote submission returns a Task", status == 200
                    and "task" in result.get("result", {}), result)
        return result["result"]["task"]

    status, card = _request(alpha, "/.well-known/agent-card.json")
    ctx.check("authenticated AgentCard advertises the closed JSON-RPC profile",
              status == 200 and card.get("supportedInterfaces", [{}])[0].get("protocolBinding") == "JSONRPC"
              and card.get("supportedInterfaces", [{}])[0].get("protocolVersion") == "1.0"
              and card.get("securitySchemes", {}).get("owner_mtls") == {"mtlsSecurityScheme": {}}
              and {entry["uri"] for entry in card.get("capabilities", {}).get("extensions", [])
                   if entry.get("required")} == {_DELEGATION, _PROVENANCE}, card)
    before = _remote_task_rows(db)
    for headers, expected in (({**_RPC_HEADERS, "A2A-Extensions": _DELEGATION}, -32008),
                              ({**_RPC_HEADERS, "A2A-Version": "0.3"}, -32009)):
        status, result = _rpc(alpha, "SendMessage", _message(first), headers=headers)
        ctx.check("unsupported protocol negotiation is rejected before persistence",
                  status == 200 and result.get("error", {}).get("code") == expected
                  and _remote_task_rows(db) == before, result)
    task = send(alpha, first)
    provenance = task.get("metadata", {}).get(_PROVENANCE, {})
    ctx.check("destination refusal distinguishes received from authorization and start",
              task.get("status", {}).get("state") == "TASK_STATE_REJECTED"
              and provenance.get("local_claim") is None and provenance.get("local_run_id") is None
              and provenance.get("origin_claim") == first["origin_claim"]
              and provenance.get("revision") == 2
              and [receipt.get("fact") for receipt in provenance.get("receipts", [])]
                  == ["received", "authorization_refused"]
              and provenance["receipts"][-1].get("detail") == {"reason": "execution_unavailable"}, task)
    canonical = json.dumps(first, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    ctx.check("receipt binds the complete immutable business digest",
              provenance.get("input_digest") == hashlib.sha256(canonical).hexdigest(), provenance)
    replay = send(alpha, first, "different-message-id")
    ctx.check("message IDs do not change durable business replay", replay == task, replay)

    race = {**first, "idempotency_key": "job-race", "nonce": "nonce-race"}
    def racing_sender():
        with closing(_client(ca, gateway.port, leaves["cloud"])) as client:
            return _rpc(client, "SendMessage", _message(race))
    with ThreadPoolExecutor(max_workers=2) as pool:
        results = list(pool.map(lambda _: racing_sender(), range(2)))
    race_ids = [result.get("result", {}).get("task", {}).get("id") for _, result in results]
    ctx.check("concurrent first submissions allocate one durable task",
              all(status == 200 for status, _ in results) and race_ids[0] is not None
              and race_ids[0] == race_ids[1]
              and sum(row[1] == "job-race" for row in _remote_task_rows(db)) == 1, results)

    before = _remote_task_rows(db)
    conflicts = [("cross-repository key reuse", {**first, "repository": "owner/other"}),
                 ("changed head with same key", {**first, "head_sha": "b" * 40}),
                 ("nonce reuse under a new key", {**first, "idempotency_key": "job-other-key"})]
    for label, changed in conflicts:
        status, result = _rpc(alpha, "SendMessage", _message(changed))
        ctx.check(label + " is refused without rewriting history",
                  status == 200 and result.get("error", {}).get("code") == -32602
                  and _remote_task_rows(db) == before, result)

    lost = {**first, "idempotency_key": "job-lost-ack", "nonce": "nonce-lost-ack"}
    with closing(_client(ca, gateway.port, leaves["cloud"])) as client:
        raw = json.dumps({"jsonrpc": "2.0", "id": "lost-ack", "method": "SendMessage",
                          "params": _message(lost)})
        client.request("POST", "/rpc", body=raw.encode(), headers=_RPC_HEADERS)
        deadline = time.monotonic() + 5
        observed = []
        while time.monotonic() < deadline:
            observed = [row for row in _remote_task_rows(db) if row[1] == "job-lost-ack"]
            if observed:
                break
            time.sleep(0.01)
        ctx.require("destination commits before the intentionally unread acknowledgment",
                    len(observed) == 1, observed)
        # Closing without getresponse models the response being lost to the origin.
    with closing(_client(ca, gateway.port, leaves["cloud"])) as client:
        recovered = send(client, lost)
    ctx.check("fresh TLS connection reconciles a lost acknowledgment without a new task",
              recovered.get("id") == observed[0][0]
              and sum(row[1] == "job-lost-ack" for row in _remote_task_rows(db)) == 1, recovered)

    for method in ("GetTask", "CancelTask", "SubscribeToTask"):
        status, foreign = _rpc(beta, method, {"id": task["id"]})
        _, missing = _rpc(beta, method, {"id": "remote-task-missing"})
        ctx.check(f"another enrolled principal cannot {method} a task by ID",
                  status == 200 and foreign == missing
                  and foreign.get("error", {}).get("code") == -32001, foreign)
    _, empty = _rpc(beta, "ListTasks", {"pageSize": 1, "historyLength": 0})
    ctx.check("listing filters unauthorized tasks before totals or cursors",
              empty.get("result") == {"tasks": [], "totalSize": 0, "pageSize": 1, "nextPageToken": ""}, empty)
    _, foreign_cursor = _rpc(beta, "ListTasks", {"pageSize": 1, "pageToken": task["id"]})
    ctx.check("a foreign pagination cursor grants no task access",
              foreign_cursor.get("error", {}).get("code") == -32602, foreign_cursor)
    for method, expected in (("CancelTask", -32002), ("SubscribeToTask", -32004)):
        _, result = _rpc(alpha, method, {"id": task["id"]})
        ctx.check(f"terminal refusal does not fabricate {method} success",
                  result.get("error", {}).get("code") == expected, result)

    before = _remote_task_rows(db)
    expired = {**first, "idempotency_key": "expired-first", "nonce": "expired-nonce",
               "issued_at": "2000-01-01T00:00:00Z", "expires_at": "2000-01-01T00:10:00Z"}
    dgx = {key: value for key, value in first.items() if key != "question"}
    dgx.update(sender="agent-second", job="verify_pr", pull_request=42,
               idempotency_key="job-dgx", nonce="nonce-dgx")
    invalid = [("expired first submission", alpha, expired),
               ("worker-selected command", alpha, {**first, "command": "whoami"}),
               ("DGX2 memory context", beta, {**dgx, "context": {"source_ref": "note:private",
                    "source_revision": "v1", "text": "Selected note",
                    "sha256": hashlib.sha256(b"Selected note").hexdigest()}}),
               ("DGX2 review question", beta, {**dgx, "question": ""}),
               ("spoofed sender in business data", beta, first),
               ("wrong audience domain", alpha, {**first, "trust_domain": "another-owner"})]
    for label, client, invalid_job in invalid:
        _, result = _rpc(client, "SendMessage", _message(invalid_job))
        ctx.check(label + " is refused before durable submission",
                  "error" in result and _remote_task_rows(db) == before, result)
    duplicate = json.dumps({"jsonrpc": "2.0", "id": "duplicate", "method": "SendMessage",
                            "params": _message(first)})
    duplicate = duplicate.replace('"sender": "agent-cloud"',
                                  '"sender": "agent-second", "sender": "agent-cloud"')
    _, result = _rpc(alpha, "SendMessage", {}, raw=duplicate)
    ctx.check("duplicate authority fields are rejected before JSON object collapse",
              "error" in result and _remote_task_rows(db) == before, result)

    ids, cursor, pages = [], None, []
    for _ in range(4):
        params = {"pageSize": 1, "historyLength": 0, "includeArtifacts": False}
        if cursor:
            params["pageToken"] = cursor
        _, result = _rpc(alpha, "ListTasks", params)
        page = result.get("result", {})
        pages.append(page)
        ids.extend(entry["id"] for entry in page.get("tasks", []))
        cursor = page.get("nextPageToken")
        if not cursor:
            break
    ctx.check("bounded pagination returns each authorized task exactly once",
              len(ids) == len(set(ids)) == 3 and not cursor
              and all(page.get("totalSize") == 3 for page in pages), pages)
    beta_task = send(beta, dgx)
    with closing(sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True)) as conn:
        counts = {table: conn.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
                  for table in ("session_claims", "harness_session_attachments", "dispatch_outcomes")}
    ctx.check("rejected remote jobs create no local claims or execution artifacts",
              all(count == 0 for count in counts.values()), counts)
    return dgx, beta_task


def _origin_submissions(ctx, origin, origin_config, origin_path, gateway, ca, leaves,
                        root, McpSession, make_fixture_repo, free_port):
    """Exercise real origin CLI + MCP claim + gateway; inject lost TLS replies."""
    repo = make_fixture_repo(root)
    subprocess.run(["git", "-C", str(repo), "remote", "set-url", "origin",
                    "https://github.com/owner/example.git"], check=True, env=origin.env)
    head = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"],
                                   env=origin.env, text=True).strip()
    origin.start()
    source = McpSession(origin.url, {"X-Tachi-Workspace-Root": str(repo),
                                   "X-Tachi-Agent-Identity": "agent-cloud"})
    lease = (datetime.now(timezone.utc) + timedelta(minutes=10)).isoformat().replace("+00:00", "Z")
    ok, claim, raw = source.call("tachi_task", action="claim", claim_scope=["src/egress.rs"],
        claim_mode="read_only", claim_role="reviewer", worktree_path=str(repo),
        expected_head=head, lease_expires_at=lease, issue_ref="owner/example#1940")
    ctx.require("origin obtains its WorkClaim through actual MCP", ok, claim if ok else raw)
    db = origin.app_home / "global" / "tachi-memory.db"
    with closing(sqlite3.connect(db)) as conn:
        project = conn.execute("SELECT canonical_project FROM session_claims WHERE claim_id=?",
                               (claim["claim_id"],)).fetchone()[0]

    # This fault proxy owns the enrolled test certificate, forwards to the real
    # gateway and drops only its response. It is not a production relay model.
    control = {"mode": "pass", "bodies": [], "http": 0}
    committed, release = threading.Event(), threading.Event()

    class Proxy(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def forward(self, method, body=None):
            control["http"] += 1
            client = _client(ca, gateway.port, leaves["cloud"])
            try:
                return _request(client, self.path, dict(self.headers), method, body)
            finally:
                client.close()

        def reply(self, status, payload):
            encoded = json.dumps(payload).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/a2a+json")
            self.send_header("A2A-Version", "1.0")
            self.send_header("A2A-Extensions", f"{_DELEGATION},{_PROVENANCE}")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

        def do_GET(self):
            response = self.forward("GET")
            hook = control.pop("after_health", None)
            if hook is not None:
                hook()
            self.reply(*response)

        def do_POST(self):
            body = self.rfile.read(int(self.headers["Content-Length"]))
            control["bodies"].append(body)
            status, payload = self.forward("POST", body)
            mode = control["mode"]
            if mode == "hold_once":
                control["mode"] = "pass"
                committed.set()
                release.wait(timeout=20)
            if mode in ("drop_once", "drop_all", "hold_once"):
                if mode == "drop_once":
                    control["mode"] = "pass"
                self.close_connection = True
                self.connection.shutdown(socket.SHUT_RDWR)
                self.connection.close()
                return
            if mode == "wrong_receipt":
                payload["result"]["task"]["metadata"][_PROVENANCE]["input_digest"] = "f" * 64
            self.reply(status, payload)

    proxy = ThreadingHTTPServer(("127.0.0.1", free_port()), Proxy)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = context.maximum_version = ssl.TLSVersion.TLSv1_3
    context.load_cert_chain(leaves["server"]["certificate"], leaves["server"]["private_key"])
    context.load_verify_locations(cafile=str(ca))
    context.verify_mode = ssl.CERT_REQUIRED
    proxy.socket = context.wrap_socket(proxy.socket, server_side=True)
    thread = threading.Thread(target=proxy.serve_forever, daemon=True)
    thread.start()
    original = json.loads(json.dumps(origin_config))
    configured = json.loads(json.dumps(origin_config))
    configured["origin_database"] = str(db)
    configured["peers"][0]["endpoint"] = f"https://localhost:{proxy.server_port}"
    configured["peers"][0]["outbound"] = [{"repository": "owner/example", "canonical_project": project,
                                          "jobs": ["review_revision", "verify_pr"]}]
    _private_text(origin_path, json.dumps(configured))

    def cli(action, *args, success=True):
        result = _run(ctx, origin, f"origin {action} completes", [str(origin.tachi), "a2a", "remote",
                      action, "--config", str(origin_path), *args])
        if success:
            ctx.require(f"origin {action} succeeds", result.returncode == 0, _output(result))
            return json.loads(result.stdout)
        return result

    def request(case, **changes):
        now = datetime.now(timezone.utc).replace(microsecond=0)
        job = {"kind": "tachi.remote_job/v1", "sender": "agent-cloud", "recipient": "agent-mac",
               "trust_domain": configured["trust_domain"], "idempotency_key": f"origin-{case}",
               "nonce": f"nonce-origin-{case}", "issued_at": now.isoformat().replace("+00:00", "Z"),
               "expires_at": (now + timedelta(minutes=10)).isoformat().replace("+00:00", "Z"),
               "repository": "owner/example", "head_sha": head,
               "origin_claim": {"claim_id": claim["claim_id"], "transition_version": claim["transition_version"]},
               "job": "review_revision", "question": "Private selected origin review question", **changes}
        path = root / f"origin-{case}.json"
        _private_text(path, json.dumps(job))
        return path

    def prepare(case, **changes):
        path = request(case, **changes)
        return cli("submit", "--request", str(path), "--prepare-only"), path

    def task_count(key):
        with closing(sqlite3.connect(gateway.app_home / "global.db")) as conn:
            return conn.execute("SELECT COUNT(*) FROM a2a_envelopes WHERE issuer_agent_identity_id='agent-cloud' AND idempotency_key=?",
                                (key,)).fetchone()[0]

    process = None
    authority_gateway = None
    try:
        pending, path = prepare("positive")
        ctx.check("origin freezes the input before all network I/O", pending["current_state"] == "pending"
                  and pending["attempts"] == 0 and control["http"] == 0 and "body" not in pending, pending)
        refused = cli("resume", "--submission", pending["submission_id"])
        ctx.check("real TLS refusal binds one origin submission to one destination task",
                  refused["current_state"] == "refused" and refused["attempts"] == 1
                  and refused["remote_receipt"]["metadata"][_PROVENANCE]["input_digest"] == pending["body_digest"]
                  and task_count(pending["idempotency_key"]) == 1, refused)
        before = control["http"]
        replay = cli("submit", "--request", str(path))
        ctx.check("terminal origin replay performs no network I/O", replay == refused and control["http"] == before,
                  {"same_snapshot": replay == refused, "http_unchanged": control["http"] == before})
        changed = json.loads(path.read_text())
        changed["question"] = "Different selected input"
        _private_text(path, json.dumps(changed))
        result = cli("submit", "--request", str(path), success=False)
        ctx.check("origin rejects same-key changed payload before I/O", result.returncode != 0
                  and control["http"] == before, _output(result))

        control["mode"] = "drop_once"
        pending, _ = prepare("lost-ack")
        start = len(control["bodies"])
        recovered = cli("resume", "--submission", pending["submission_id"])
        ctx.check("lost acknowledgment retries identical bytes and recovers the original task",
                  recovered["current_state"] == "refused" and recovered["attempts"] == 2
                  and len(control["bodies"][start:]) == 2 and len(set(control["bodies"][start:])) == 1
                  and task_count(pending["idempotency_key"]) == 1,
                  {"state": recovered["current_state"], "attempts": recovered["attempts"],
                   "destination_tasks": task_count(pending["idempotency_key"])})

        control["mode"] = "drop_all"
        pending, _ = prepare("exhausted")
        start = len(control["bodies"])
        exhausted = cli("resume", "--submission", pending["submission_id"])
        before = control["http"]
        again = cli("resume", "--submission", pending["submission_id"])
        ctx.check("three lost replies exhaust one frozen submission without fabricating refusal",
                  exhausted["current_state"] == "unknown" and exhausted["attempts"] == 3
                  and not exhausted["retry_allowed"] and exhausted["remote_receipt"] is None
                  and again == exhausted and control["http"] == before
                  and len(control["bodies"][start:]) == 3 and len(set(control["bodies"][start:])) == 1
                  and task_count(pending["idempotency_key"]) == 1, exhausted)

        control["mode"] = "wrong_receipt"
        pending, _ = prepare("wrong-receipt")
        wrong = cli("resume", "--submission", pending["submission_id"])
        ctx.check("an authenticated but mismatched receipt remains unresolved",
                  wrong["current_state"] == "unknown" and wrong["attempts"] == 1
                  and wrong["last_error"] == "invalid_response" and wrong["remote_receipt"] is None
                  and not wrong["retry_allowed"], wrong)

        control["mode"] = "hold_once"
        pending, _ = prepare("crash")
        start = len(control["bodies"])
        process = subprocess.Popen([str(origin.tachi), "a2a", "remote", "resume", "--config", str(origin_path),
                                    "--submission", pending["submission_id"]], env=origin.env, cwd=origin.home,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        ctx.require("destination commits before simulated origin crash", committed.wait(timeout=15), "real gateway reply held")
        process.kill()
        process.communicate(timeout=5)
        release.set()
        crashed = cli("status", "--submission", pending["submission_id"])
        early = cli("resume", "--submission", pending["submission_id"])
        ctx.check("restart retains the in-flight attempt and fences early recovery",
                  crashed == early and early["current_state"] == "in_flight" and early["attempts"] == 1
                  and len(control["bodies"][start:]) == 1, early)
        until = datetime.fromisoformat(early["attempt_until"].replace("Z", "+00:00"))
        time.sleep(max(0, (until - datetime.now(timezone.utc)).total_seconds()) + 0.05)
        resumed = cli("resume", "--submission", pending["submission_id"])
        ctx.check("crashed origin recovers through the same durable key after reservation expiry",
                  resumed["current_state"] == "refused" and resumed["attempts"] == 2
                  and len(control["bodies"][start:]) == 2 and len(set(control["bodies"][start:])) == 1
                  and task_count(pending["idempotency_key"]) == 1, resumed)

        pending, _ = prepare("endpoint-change")
        changed_config = json.loads(json.dumps(configured))
        changed_config["peers"][0]["endpoint"] = f"https://localhost:{free_port()}"
        _private_text(origin_path, json.dumps(changed_config))
        before = control["http"]
        stopped = cli("resume", "--submission", pending["submission_id"])
        ctx.check("existing submission cannot be redirected to another endpoint",
                  stopped["current_state"] == "blocked" and stopped["attempts"] == 0
                  and control["http"] == before, stopped)
        _private_text(origin_path, json.dumps(configured))

        changed_config = json.loads(json.dumps(configured))
        changed_config["peers"][0]["public_key_sha256"] = leaves["second"]["public_key_sha256"]
        _private_text(origin_path, json.dumps(changed_config))
        pending, _ = prepare("wrong-pin")
        before = control["http"]
        pin = cli("resume", "--submission", pending["submission_id"])
        ctx.check("outbound task bytes never reach a destination with the wrong enrolled key",
                  pin["current_state"] == "unknown" and pin["attempts"] == 3 and control["http"] == before, pin)
        _private_text(origin_path, json.dumps(configured))

        before = control["http"]
        for case, change in (("forged-claim", {"origin_claim": {"claim_id": "unowned", "transition_version": 0}}),
                             ("wrong-head", {"head_sha": "f" * 40}),
                             ("dgx-context", {"job": "verify_pr", "pull_request": 1940})):
            result = cli("submit", "--request", str(request(case, **change)), success=False)
            ctx.check(f"origin refuses {case} before any HTTP", result.returncode != 0
                      and control["http"] == before, _output(result))

        pending, _ = prepare("released-claim")
        ok, released, raw = source.call("tachi_task", action="release", claim_id=claim["claim_id"],
                                       transition_version=claim["transition_version"])
        ctx.require("origin WorkClaim releases through MCP", ok, released if ok else raw)
        before = control["http"]
        stale = cli("resume", "--submission", pending["submission_id"])
        ctx.check("current claim release blocks already prepared egress",
                  stale["current_state"] == "blocked" and stale["attempts"] == 0
                  and stale["last_error"] == "origin_authority_changed" and control["http"] == before, stale)

        # Claims/submissions and key retirement may use separate existing stores.
        ok, claim, raw = source.call("tachi_task", action="claim", claim_scope=["src/egress.rs"],
            claim_mode="read_only", claim_role="reviewer", worktree_path=str(repo), expected_head=head,
            lease_expires_at=lease, issue_ref="owner/example#1940")
        ctx.require("fresh origin claim for revocation boundary", ok, claim if ok else raw)
        authority_gateway = type(origin)(origin.tachi, root / "origin-authority-process", free_port())
        authority_config = {**configured, "bind": f"127.0.0.1:{authority_gateway.port}"}
        authority_path = root / "origin-authority.json"
        _private_text(authority_path, json.dumps(authority_config))
        _start_gateway(ctx, authority_gateway, authority_path)
        destination = _client(ca, authority_gateway.port, leaves["server"])
        try:
            status, admitted = _request(destination)
        finally:
            destination.close()
        ctx.require("destination key has a real admission in origin gateway authority", status == 200,
                    {"status": status, "peer": admitted.get("peer")})
        after_tls, _ = prepare("revoke-after-health")
        before_tls, _ = prepare("revoke-before-attempt")

        def revoke_after_health():
            control["revocation"] = subprocess.run([str(origin.tachi), "a2a", "remote", "revoke",
                "--config", str(origin_path), "--admission", admitted["peer"]["admission_id"]],
                env=origin.env, cwd=origin.home, capture_output=True, timeout=15)

        control["after_health"] = revoke_after_health
        start = len(control["bodies"])
        denied = cli("resume", "--submission", after_tls["submission_id"])
        ctx.require("operator revocation during health commits in gateway ledger",
                    control["revocation"].returncode == 0, _output(control["revocation"]))
        with closing(sqlite3.connect(db)) as conn:
            origin_revocations = conn.execute("SELECT COUNT(*) FROM identity_admission_verification_revocations").fetchone()[0]
        with closing(sqlite3.connect(origin.app_home / "global.db")) as conn:
            gateway_revocations = conn.execute("SELECT COUNT(*) FROM identity_admission_verification_revocations").fetchone()[0]
        ctx.check("gateway revocation after TLS prevents POST even when claim DB has no revocation",
                  denied["current_state"] == "unknown" and denied["attempts"] == 1
                  and denied["last_error"] == "destination_authority_changed" and not denied["retry_allowed"]
                  and len(control["bodies"]) == start and origin_revocations == 0 and gateway_revocations == 1, denied)
        before = control["http"]
        denied = cli("resume", "--submission", before_tls["submission_id"], success=False)
        fresh = cli("submit", "--request", str(request("revoked-key")), success=False)
        untouched = cli("status", "--submission", before_tls["submission_id"])
        ctx.check("gateway key retirement blocks new and already prepared submissions before any HTTP",
                  denied.returncode != 0 and fresh.returncode != 0 and untouched["attempts"] == 0
                  and control["http"] == before, {"resume": _output(denied), "submit": _output(fresh)})
    finally:
        release.set()
        if process is not None and process.poll() is None:
            process.kill()
            process.communicate(timeout=5)
        proxy.shutdown()
        proxy.server_close()
        thread.join(timeout=5)
        origin.stop()
        if authority_gateway is not None:
            authority_gateway.stop()
        _private_text(origin_path, json.dumps(original))


def _admit_only_lifecycle(ctx, root, Daemon, McpSession, make_fixture_repo, free_port, ca, leaves):
    """Verify real-TLS AdmitOnly admission, slot cap, cancel, cross-peer isolation, and GetTask reconcile."""
    admit_root = root / "admit-only"
    admit_root.mkdir(mode=0o700)
    gw = Daemon(ctx.daemon.tachi, admit_root / "gateway", free_port())
    orig = Daemon(ctx.daemon.tachi, admit_root / "origin", free_port())
    gw.app_home.chmod(0o700)
    orig.app_home.chmod(0o700)
    repo = make_fixture_repo(admit_root).resolve()
    subprocess.run(["git", "-C", str(repo), "remote", "set-url", "origin",
                    "https://github.com/owner/admit.git"], check=True, env=orig.env)
    head = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"],
                                   env=orig.env, text=True).strip()
    subprocess.run(["git", "-C", str(repo), "update-ref", "refs/pull/99/head", head],
                   check=True, env=orig.env)
    canonical_project = hashlib.sha256(b"admit-project").hexdigest()
    domain = "dogfood:remote-admit"

    gw_config = {
        "schema_version": "tachi.remote_gateway.v1",
        "agent_identity_id": "agent-mac",
        "host_id": "host-mac",
        "issuer_id": "issuer-admit-gw",
        "trust_domain": domain,
        "home": str(gw.app_home),
        "bind": f"127.0.0.1:{gw.port}",
        "public_endpoint": f"https://localhost:{gw.port}",
        "certificate": leaves["server"]["certificate"],
        "private_key": leaves["server"]["private_key"],
        "peer_ca": str(ca),
        "execution_mode": "admit_only",
        "destinations": [{
            "repository": "owner/admit",
            "canonical_project": canonical_project,
            "repo_root": str(repo),
            "allowed_jobs": ["review_revision", "verify_pr"],
            "review_profile": "codex_sol_review",
            "verify_policy": "dgx2_acceptance_v1",
            "resource_slot": "slot-mac-1",
            "max_concurrent_runs": 1,
            "lease_ttl_secs": 600,
        }],
        "peers": [
            {
                "agent_identity_id": "agent-cloud",
                "host_id": "host-cloud",
                "public_key_sha256": leaves["cloud"]["public_key_sha256"],
                "repositories": [{"repository": "owner/admit", "jobs": ["review_revision", "verify_pr"]}],
            },
            {
                "agent_identity_id": "agent-second",
                "host_id": "host-second",
                "public_key_sha256": leaves["second"]["public_key_sha256"],
                "repositories": [{"repository": "owner/admit", "jobs": ["review_revision", "verify_pr"]}],
            },
        ],
    }
    orig_config = {
        "schema_version": "tachi.remote_gateway.v1",
        "agent_identity_id": "agent-cloud",
        "host_id": "host-cloud",
        "issuer_id": "issuer-admit-orig",
        "trust_domain": domain,
        "home": str(orig.app_home),
        "origin_database": str(orig.app_home / "global" / "tachi-memory.db"),
        "bind": f"127.0.0.1:{orig.port}",
        "certificate": leaves["cloud"]["certificate"],
        "private_key": leaves["cloud"]["private_key"],
        "peer_ca": str(ca),
        "peers": [{
            "agent_identity_id": "agent-mac",
            "host_id": "host-mac",
            "public_key_sha256": leaves["server"]["public_key_sha256"],
            "endpoint": f"https://localhost:{gw.port}",
            "outbound": [{
                "repository": "owner/admit",
                "canonical_project": "",
                "jobs": ["review_revision", "verify_pr"],
            }],
        }],
    }
    gw_path = admit_root / "gateway.json"
    orig_path = admit_root / "origin.json"
    _private_text(gw_path, json.dumps(gw_config))
    orig.start()
    alpha = beta = None
    try:
        source = McpSession(orig.url, {"X-Tachi-Workspace-Root": str(repo),
                                       "X-Tachi-Agent-Identity": "agent-cloud"})
        lease = (datetime.now(timezone.utc) + timedelta(minutes=10)).isoformat().replace("+00:00", "Z")
        ok, claim, raw = source.call("tachi_task", action="claim", claim_scope=["src/main.rs"],
            claim_mode="read_only", claim_role="reviewer", worktree_path=str(repo),
            expected_head=head, lease_expires_at=lease, issue_ref="owner/admit#rev")
        ctx.require("admit-only origin obtains WorkClaim", ok, claim if ok else raw)
        with closing(sqlite3.connect(orig.app_home / "global" / "tachi-memory.db")) as conn:
            orig_project = conn.execute("SELECT canonical_project FROM session_claims WHERE claim_id=?",
                                        (claim["claim_id"],)).fetchone()[0]
        orig_config["peers"][0]["outbound"][0]["canonical_project"] = orig_project
        _private_text(orig_path, json.dumps(orig_config))

        _start_gateway(ctx, gw, gw_path)
        gw_db = gw.app_home / "global.db"
        alpha = _client(ca, gw.port, leaves["cloud"])
        beta = _client(ca, gw.port, leaves["second"])

        now = datetime.now(timezone.utc).replace(microsecond=0)
        req_file = admit_root / "job-admit.json"
        req_payload = {
            "kind": "tachi.remote_job/v1",
            "sender": "agent-cloud",
            "recipient": "agent-mac",
            "trust_domain": domain,
            "idempotency_key": "admit-job-1",
            "nonce": "admit-nonce-1",
            "issued_at": now.isoformat().replace("+00:00", "Z"),
            "expires_at": (now + timedelta(minutes=10)).isoformat().replace("+00:00", "Z"),
            "repository": "owner/admit",
            "head_sha": head,
            "origin_claim": {"claim_id": claim["claim_id"], "transition_version": claim["transition_version"]},
            "job": "review_revision",
            "question": "Review committed head in admit_only mode.",
        }
        _private_text(req_file, json.dumps(req_payload))

        def orig_cli(*args):
            res = _run(ctx, orig, f"admit-only cli {' '.join(args)}",
                       [str(orig.tachi), "a2a", "remote", *args])
            ctx.require(f"admit-only cli {' '.join(args)} succeeds", res.returncode == 0, _output(res))
            return json.loads(res.stdout)

        submitted = orig_cli("submit", "--config", str(orig_path), "--request", str(req_file))
        prov = submitted["remote_receipt"]["metadata"][_PROVENANCE]
        ctx.check("admit_only admits valid commit into TASK_STATE_SUBMITTED with zero workers spawned",
                  submitted["current_state"] == "submitted"
                  and submitted["remote_receipt"]["status"]["state"] == "TASK_STATE_SUBMITTED"
                  and prov["local_claim"] is not None and prov["local_run_id"] is not None
                  and [r["fact"] for r in prov["receipts"]] == ["received", "admitted"], submitted)
        with closing(sqlite3.connect(gw_db)) as conn:
            run_count = conn.execute("SELECT COUNT(*) FROM a2a_remote_runs").fetchone()[0]
            claim_count = conn.execute("SELECT COUNT(*) FROM session_claims WHERE state='active'").fetchone()[0]
            exec_count = conn.execute("SELECT COUNT(*) FROM dispatch_outcomes").fetchone()[0]
        ctx.check("admit_only creates 1 run, 1 active local claim, and 0 worker dispatches",
                  run_count == 1 and claim_count == 1 and exec_count == 0,
                  {"runs": run_count, "claims": claim_count, "dispatches": exec_count})

        replayed = orig_cli("submit", "--config", str(orig_path), "--request", str(req_file))
        with closing(sqlite3.connect(gw_db)) as conn:
            after_replay_runs = conn.execute("SELECT COUNT(*) FROM a2a_remote_runs").fetchone()[0]
        ctx.check("admit_only replay returns original task without creating a second run",
                  replayed["remote_task_id"] == submitted["remote_task_id"] and after_replay_runs == 1,
                  replayed)

        wrong_head = {**req_payload, "idempotency_key": "admit-wrong-head", "nonce": "admit-nonce-wh",
                      "head_sha": "f" * 40}
        _, wh_res = _rpc(alpha, "SendMessage", _message(wrong_head))
        wh_task = wh_res.get("result", {}).get("task", {})
        ctx.check("admit_only rejects missing head_sha cleanly",
                  wh_task.get("status", {}).get("state") == "TASK_STATE_REJECTED"
                  and wh_task.get("metadata", {}).get(_PROVENANCE, {}).get("receipts", [{}])[-1].get("detail")
                      == {"reason": "head_not_found"}, wh_res)

        subprocess.run(["git", "-C", str(repo), "update-ref", "refs/tags/refs/pull/98/head", head],
                       check=True, env=orig.env)
        unseeded_pr = {**req_payload, "job": "verify_pr", "pull_request": 98,
                       "idempotency_key": "admit-pr-unseeded", "nonce": "admit-nonce-pr98"}
        unseeded_pr.pop("question")
        _, pr98_res = _rpc(alpha, "SendMessage", _message(unseeded_pr))
        pr98_task = pr98_res.get("result", {}).get("task", {})
        ctx.check("admit_only rejects verify_pr when trusted PR head ref is missing or mismatched",
                  pr98_task.get("status", {}).get("state") == "TASK_STATE_REJECTED"
                  and pr98_task.get("metadata", {}).get(_PROVENANCE, {}).get("receipts", [{}])[-1].get("detail")
                      == {"reason": "head_not_found"}, pr98_res)

        slot_full = {**req_payload, "job": "verify_pr", "pull_request": 99,
                     "idempotency_key": "admit-slot-full", "nonce": "admit-nonce-sf"}
        slot_full.pop("question")
        _, sf_res = _rpc(alpha, "SendMessage", _message(slot_full))
        sf_task = sf_res.get("result", {}).get("task", {})
        ctx.check("admit_only rejects concurrent excess with resource_exhausted",
                  sf_task.get("status", {}).get("state") == "TASK_STATE_REJECTED"
                  and sf_task.get("metadata", {}).get(_PROVENANCE, {}).get("receipts", [{}])[-1].get("detail")
                      == {"reason": "resource_exhausted"}, sf_res)

        task_id = submitted["remote_task_id"]
        for method in ("GetTask", "CancelTask"):
            _, foreign = _rpc(beta, method, {"id": task_id})
            ctx.check(f"admit_only cross-peer {method} returns TaskNotFound",
                      foreign.get("error", {}).get("code") == -32001, foreign)

        _, canceled = _rpc(alpha, "CancelTask", {"id": task_id})
        cancel_res = canceled.get("result", {})
        ctx.check("admit_only CancelTask adds cancel_requested receipt while keeping TASK_STATE_SUBMITTED",
                  cancel_res.get("status", {}).get("state") == "TASK_STATE_SUBMITTED"
                  and [r.get("fact") for r in cancel_res.get("metadata", {}).get(_PROVENANCE, {}).get("receipts", [])]
                      == ["received", "admitted", "cancel_requested"], canceled)

        reconciled = orig_cli("status", "--config", str(orig_path), "--submission", submitted["submission_id"], "--reconcile")
        ctx.check("origin status --reconcile updates local snapshot via GetTask without resending",
                  reconciled["current_state"] == "submitted" and reconciled["attempts"] == 1
                  and reconciled["remote_receipt"]["metadata"][_PROVENANCE]["revision"] == 3, reconciled)
    finally:
        if alpha is not None:
            alpha.close()
        if beta is not None:
            beta.close()
        gw.stop()
        orig.stop()


def register_scenarios(scenario, Daemon, free_port, McpSession, make_fixture_repo):
    @scenario("remote-admission", "a2a-remote", "identity", shared_daemon=False)
    def remote_admission_enforces_tls_identity_and_revocation(ctx):
        root = ctx.daemon.home / f"remote-admission-{ctx.tag}"
        root.mkdir(mode=0o700)
        gateway = Daemon(ctx.daemon.tachi, root / "gateway", free_port())
        origin = Daemon(ctx.daemon.tachi, root / "origin", free_port())
        gateway.app_home.chmod(0o700)
        origin.app_home.chmod(0o700)
        ca, leaves = _pki(ctx, gateway, root / "pki")
        domain = "dogfood:remote"
        peers = [("cloud", "agent-cloud", "host-cloud"),
                 ("second", "agent-second", "host-second"),
                 ("expired", "agent-expired", "host-expired")]
        gateway_config = {
            "schema_version": "tachi.remote_gateway.v1", "agent_identity_id": "agent-mac",
            "host_id": "host-mac", "issuer_id": "issuer-dogfood", "trust_domain": domain,
            "home": str(gateway.app_home), "bind": f"127.0.0.1:{gateway.port}",
            "public_endpoint": f"https://localhost:{gateway.port}",
            "certificate": leaves["server"]["certificate"],
            "private_key": leaves["server"]["private_key"], "peer_ca": str(ca),
            "peers": [{"agent_identity_id": identity, "host_id": host,
                       "public_key_sha256": leaves[name]["public_key_sha256"],
                       "repositories": [{"repository": repo, "jobs": ["review_revision", "verify_pr"]}
                                        for repo in ("owner/example", "owner/other")]}
                      for name, identity, host in peers],
        }
        origin_config = {
            **gateway_config, "agent_identity_id": "agent-cloud", "host_id": "host-cloud",
            "issuer_id": "issuer-origin", "home": str(origin.app_home),
            "bind": f"127.0.0.1:{origin.port}", "certificate": leaves["cloud"]["certificate"],
            "private_key": leaves["cloud"]["private_key"],
            "peers": [{"agent_identity_id": "agent-mac", "host_id": "host-mac",
                       "public_key_sha256": leaves["server"]["public_key_sha256"],
                       "endpoint": f"https://localhost:{gateway.port}"}],
        }
        gateway_path, origin_path = root / "gateway.json", root / "origin.json"
        _private_text(gateway_path, json.dumps(gateway_config))
        _private_text(origin_path, json.dumps(origin_config))
        db = gateway.app_home / "global.db"
        clients = []
        pin_server = pin_thread = None

        def connection(leaf):
            client = _client(ca, gateway.port, leaf)
            clients.append(client)
            return client

        def cli(daemon, action, config, *args):
            return _run(ctx, daemon, f"remote {action} completes",
                        [str(daemon.tachi), "a2a", "remote", action, "--config", str(config), *args])

        def health(client, identity, host):
            status, payload = _request(client)
            ctx.require(f"authenticated health for {identity}", status == 200
                        and isinstance(payload, dict), {"status": status, "body": payload})
            peer = payload.get("peer", {})
            ctx.require(f"health binds enrolled identity {identity}",
                        payload.get("schema_version") == "tachi.remote_admission.v1"
                        and payload.get("gateway") == {"agent_identity_id": "agent-mac",
                            "host_id": "host-mac", "trust_domain": domain}
                        and peer.get("agent_identity_id") == identity and peer.get("host_id") == host
                        and peer.get("verification_method") == "mtls-peer"
                        and peer.get("verification_version") == "v1"
                        and bool(peer.get("admission_id")) and bool(peer.get("connection_id"))
                        and payload.get("execution") == "unavailable", payload)
            return payload

        try:
            # Initialize the isolated store without an ordinary daemon, then
            # leave a legacy replay job that ordinary startup would quarantine.
            # Admission/revocation must not run that unrelated maintenance work.
            initialize = cli(gateway, "revoke", gateway_path, "--admission", "absent-admission")
            ctx.require("remote command initializes its isolated store", db.is_file()
                        and initialize.returncode != 0, _output(initialize))
            marker = ("remote-must-not-replay", '{"schema":"tachi.admitted_mcp_ingest.v1"}',
                      "auto_ingest_job", "2000-01-01T00:00:00.000Z")
            with closing(sqlite3.connect(db)) as conn:
                conn.execute("INSERT INTO processed_events (event_hash,event_id,worker,created_at) "
                             "VALUES (?,?,?,?)", marker)
                conn.commit()
            _start_gateway(ctx, gateway, gateway_path)
            # Give ordinary startup replay time to act before checking isolation.
            time.sleep(0.5)
            before = _snapshot(db)
            for label, leaf in (("missing client certificate", None),
                                ("same-CA unenrolled key", leaves["unenrolled"]),
                                ("enrolled but expired certificate", leaves["expired"])):
                client = connection(leaf)
                denied, evidence = _refused(client)
                client.close()
                after = _snapshot(db)
                ctx.check(f"{label} is denied before admission", denied and after == before,
                          {**evidence, "durable_admissions_unchanged": after == before})
                before = after

            alpha = connection(leaves["cloud"])
            alpha_health = health(alpha, "agent-cloud", "host-cloud")
            alpha_socket = alpha.sock
            ctx.require("authenticated connection uses TLS 1.3 keep-alive",
                        alpha_socket is not None and alpha_socket.version() == "TLSv1.3",
                        alpha_socket.version() if alpha_socket else "connection closed")
            beta = connection(leaves["second"])
            beta_health = health(beta, "agent-second", "host-second")
            status, spoofed = _request(alpha, headers={"X-Tachi-Agent-Identity": "agent-second",
                "X-Tachi-Host-Identity": "host-second", "X-Tachi-Internal-Proxy-Token": "spoofed"})
            ctx.check("another client and spoofed headers cannot replace the TLS principal",
                      status == 200 and spoofed == alpha_health
                      and alpha.sock is alpha_socket
                      and alpha_health["peer"]["admission_id"] != beta_health["peer"]["admission_id"]
                      and alpha_health["peer"]["connection_id"] != beta_health["peer"]["connection_id"],
                      {"first": alpha_health["peer"], "second": beta_health["peer"], "spoofed": spoofed})
            reconnect = connection(leaves["cloud"])
            resumed = health(reconnect, "agent-cloud", "host-cloud")
            ctx.check("reconnect creates a fresh connection-bound admission",
                      all(resumed["peer"][key] != alpha_health["peer"][key]
                          for key in ("admission_id", "connection_id")), resumed["peer"])
            reconnect.close()
            mcp_client = connection(leaves["second"])
            status, response = _request(mcp_client, "/mcp", {"Content-Type": "application/json"}, "POST",
                json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
            ctx.check("remote gateway does not expose local MCP", status == 404,
                      {"status": status, "body": response})
            mcp_client.close()

            result = cli(origin, "probe", origin_path, "--peer", "agent-mac")
            ctx.require("real CLI probe reaches the enrolled gateway", result.returncode == 0,
                        _output(result))
            probed = json.loads(result.stdout)
            ctx.check("CLI probe returns the authenticated principal and unavailable execution",
                      probed.get("peer", {}).get("agent_identity_id") == "agent-cloud"
                      and probed.get("execution") == "unavailable", probed)

            pin_port = free_port()
            pin_payload = {**alpha_health, "peer": {**alpha_health["peer"],
                           "admission_id": "pin-control", "connection_id": "pin-control"}}
            pin_server, pin_thread, counts = _pin_endpoint(ca, leaves["server"], pin_port, pin_payload)
            pin_config = {**origin_config, "peers": [{**origin_config["peers"][0],
                          "endpoint": f"https://localhost:{pin_port}"}]}
            pin_path = root / "pin-control.json"
            _private_text(pin_path, json.dumps(pin_config))
            result = cli(origin, "probe", pin_path, "--peer", "agent-mac")
            ctx.require("pin test endpoint passes its positive control", result.returncode == 0
                        and counts["requests"] == 1, _output(result))
            pin_config["peers"][0]["public_key_sha256"] = leaves["second"]["public_key_sha256"]
            _private_text(pin_path, json.dumps(pin_config))
            result = cli(origin, "probe", pin_path, "--peer", "agent-mac")
            ctx.check("wrong destination SPKI is refused before any HTTP request",
                      result.returncode != 0 and counts["requests"] == 1,
                      {"returncode": result.returncode, "http_requests": counts["requests"],
                       "output": _output(result)})

            dgx_request, dgx_task = _remote_tasks(ctx, gateway, ca, leaves, alpha, beta, db)

            # Origin crash recovery intentionally spans the durable reservation.
            # Do not leave the separate revocation controls idle past the
            # gateway's bounded header wait; admit live controls afterward.
            alpha.close()
            beta.close()
            _origin_submissions(ctx, origin, origin_config, origin_path, gateway, ca, leaves,
                                root, McpSession, make_fixture_repo, free_port)
            alpha = connection(leaves["cloud"])
            alpha_health = health(alpha, "agent-cloud", "host-cloud")
            alpha_socket = alpha.sock
            beta = connection(leaves["second"])
            beta_health = health(beta, "agent-second", "host-second")

            before_revoke = _snapshot(db)
            result = cli(gateway, "revoke", gateway_path, "--admission", alpha_health["peer"]["admission_id"])
            ctx.require("operator CLI appends mTLS key revocation", result.returncode == 0
                        and json.loads(result.stdout).get("scope") == "mtls-peer-key", _output(result))
            ctx.require("revocation check retains the original live TLS connection",
                        alpha.sock is alpha_socket, "same socket before post-revocation request")
            denied, evidence = _refused(alpha)
            ctx.check("revoked key loses authority on an existing keep-alive connection", denied, evidence)
            task_rows = _remote_task_rows(db)
            rpc_status, rpc_denial = _rpc(alpha, "ListTasks", {})
            ctx.check("revocation also removes RPC read and submission authority",
                      rpc_status == 403 and _remote_task_rows(db) == task_rows, rpc_denial)
            fresh = connection(leaves["cloud"])
            denied, evidence = _refused(fresh)
            fresh.close()
            ctx.check("revoked key cannot acquire a new connection admission", denied, evidence)
            unaffected = health(beta, "agent-second", "host-second")
            ctx.check("revocation leaves a different enrolled key usable", unaffected == beta_health,
                      unaffected["peer"])
            after_revoke = _snapshot(db)
            unchanged = all(after_revoke[table] == before_revoke[table] for table in (
                "identity_admissions", "identity_admission_verification_receipts"))
            revocations = "identity_admission_verification_revocations"
            added = [row for row in after_revoke[revocations] if row not in before_revoke[revocations]]
            ctx.check("revocation preserves append-only admissions and receipts",
                      unchanged and len(added) == 1
                      and added[0][1] == alpha_health["peer"]["admission_id"],
                      {"history_unchanged": unchanged, "new_revocations": len(added)})
            with closing(sqlite3.connect(f"{db.as_uri()}?mode=ro", uri=True)) as conn:
                retained = conn.execute("SELECT event_hash,event_id,worker,created_at "
                                        "FROM processed_events WHERE event_hash=?", (marker[0],)).fetchall()
            ctx.check("gateway and revoke do not replay unrelated background jobs",
                      retained == [marker], {"unchanged_pending_job": retained == [marker]})

            for client in clients:
                client.close()
            gateway.stop()
            _start_gateway(ctx, gateway, gateway_path)
            resumed = connection(leaves["second"])
            _, readback = _rpc(resumed, "GetTask", {"id": dgx_task["id"], "historyLength": 0})
            _, replay = _rpc(resumed, "SendMessage", _message(dgx_request, "restart-replay"))
            ctx.check("gateway restart retains exact task receipts and replay identity",
                      readback.get("result") == dgx_task
                      and replay.get("result", {}).get("task") == dgx_task
                      and _remote_task_rows(db) == task_rows, {"get": readback, "replay": replay})
            resumed.close()
            gateway.stop()
            gateway_config["peers"][1]["repositories"] = []
            _private_text(gateway_path, json.dumps(gateway_config))
            _start_gateway(ctx, gateway, gateway_path)
            ungranted = connection(leaves["second"])
            _, readback = _rpc(ungranted, "GetTask", {"id": dgx_task["id"]})
            _, listed = _rpc(ungranted, "ListTasks", {})
            _, replay = _rpc(ungranted, "SendMessage", _message(dgx_request))
            ctx.check("removing a repository grant hides old tasks and prevents replay disclosure",
                      readback.get("error", {}).get("code") == -32001
                      and listed.get("result", {}).get("totalSize") == 0
                      and listed.get("result", {}).get("tasks") == []
                      and "error" in replay and _remote_task_rows(db) == task_rows,
                      {"get": readback, "list": listed, "replay": replay})
            for client in clients:
                client.close()
            clients.clear()
            gateway.stop()
            _admit_only_lifecycle(ctx, root, Daemon, McpSession, make_fixture_repo, free_port, ca, leaves)
        finally:
            for client in clients:
                client.close()
            if pin_server is not None:
                pin_server.shutdown()
                pin_server.server_close()
            if pin_thread is not None:
                pin_thread.join(timeout=5)
            gateway.stop()
        ctx.done()

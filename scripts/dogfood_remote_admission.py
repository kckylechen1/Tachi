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
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost,IP:127.0.0.1
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid,issuer
[client]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = clientAuth
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
               "-config", config, "-extensions", "server" if name == "server" else "client",
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


def register_scenarios(scenario, Daemon, free_port):
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

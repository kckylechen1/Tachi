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
from contextlib import closing
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
            "certificate": leaves["server"]["certificate"],
            "private_key": leaves["server"]["private_key"], "peer_ca": str(ca),
            "peers": [{"agent_identity_id": identity, "host_id": host,
                       "public_key_sha256": leaves[name]["public_key_sha256"]}
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
            with gateway.log.open("w") as log:
                gateway.process = subprocess.Popen(
                    [str(gateway.tachi), "a2a", "remote", "serve", "--config", str(gateway_path)],
                    env=gateway.env, cwd=gateway.home, stdout=log, stderr=subprocess.STDOUT,
                    start_new_session=True,
                )
            _wait_ready(ctx, gateway)
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

            before_revoke = _snapshot(db)
            result = cli(gateway, "revoke", gateway_path, "--admission", alpha_health["peer"]["admission_id"])
            ctx.require("operator CLI appends mTLS key revocation", result.returncode == 0
                        and json.loads(result.stdout).get("scope") == "mtls-peer-key", _output(result))
            ctx.require("revocation check retains the original live TLS connection",
                        alpha.sock is alpha_socket, "same socket before post-revocation request")
            denied, evidence = _refused(alpha)
            ctx.check("revoked key loses authority on an existing keep-alive connection", denied, evidence)
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

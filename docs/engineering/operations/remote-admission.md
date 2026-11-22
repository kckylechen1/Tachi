# Remote admission gateway

Implementation slice for #1171 under #1129. The gateway authenticates remote
peers through real mutual TLS and records `mtls-peer/v1` evidence in the existing
Product admission ledger. **It does not accept remote jobs, expose MCP, launch
workers or establish a deployed execution boundary.** #1940 and #1934 own those
subsequent integrations.

## Operator configuration

Use an explicit JSON file with `schema_version: "tachi.remote_gateway.v1"`.
The following values illustrate the shape and must be replaced with enrolled
identities, real certificate paths and public-key digests before use:

```json
{
  "schema_version": "tachi.remote_gateway.v1",
  "agent_identity_id": "agent-mac-gateway",
  "host_id": "mac-host",
  "issuer_id": "owner-gateway-issuer",
  "trust_domain": "owner-work",
  "home": "/var/lib/tachi-gateway",
  "bind": "127.0.0.1:6920",
  "certificate": "/var/lib/tachi-gateway/tls/gateway.pem",
  "private_key": "/var/lib/tachi-gateway/tls/gateway-key.pem",
  "peer_ca": "/var/lib/tachi-gateway/tls/owner-ca.pem",
  "peers": [
    {
      "agent_identity_id": "agent-cloud-lead",
      "host_id": "cloud-host",
      "public_key_sha256": "<64 lowercase hex digits of the peer DER SPKI>",
      "endpoint": null
    }
  ]
}
```

The configured home is the explicit Product runtime home; the existing store
is `home/global.db`. No project or implicit default store is selected. An older
store still requires the existing explicit `--allow-schema-migration` flag.
This method addition changes no DDL or schema version. Test against an isolated
home or an authorized stopped copy, never an incidental live user store.

The home, config and private key must belong to the gateway OS account and have
no group/other permissions (`0700` directory, `0600` files). Certificate and CA
files may be readable but must be owned by that account and not group/other
writable. Operator files are bounded to 64 KiB and opened without following a
final symlink. Unknown fields and ambiguous key/identity enrollments are refused.
The initial platform boundary uses Unix ownership/permissions; other platforms
need their own verified protection adapter before running this gateway.

An enrollment maps one public key to one stable identity and host in the
configured trust domain. Obtain the public-key digest without reading a private
key:

```bash
openssl x509 -in peer.pem -pubkey -noout |
  openssl pkey -pubin -outform DER |
  openssl dgst -sha256
```

Both sides need certificates trusted by the configured owner CA. Servers need
the endpoint DNS/IP in their certificate; peers need client-auth usage. The
chain must cover the entire fifteen-minute admission window. Connections use
TLS 1.3, with session resumption disabled so a new admission has a fresh proof
of key possession. Identity headers and model/carrier labels have no effect.

## Serve and probe

```bash
tachi a2a remote serve --config /absolute/gateway.json
tachi a2a remote probe --config /absolute/origin.json --peer agent-mac-gateway
```

The origin has its own identity/certificate/config and a reciprocal peer entry
with the destination's public-key digest and HTTPS origin in `endpoint`.
Endpoint URLs cannot contain credentials, query strings or a non-root path.
The client checks the CA, hostname and enrolled destination SPKI during TLS,
before sending an HTTP request. It disables redirects and ambient proxies,
then checks the response identity, host, domain and method/version bindings.

The only served application route is authenticated `GET /health`. It reports
the caller's own admission/connection IDs and `execution: unavailable`.
No `/mcp`, Vault or arbitrary local router is mounted. `--global-db`,
`--project-db`, `--profile` and `--daemon` are rejected for this entry point;
the explicit gateway config owns those choices. Project/user dotenv startup
and the normal daemon proxy are not used.

The gateway caps concurrent connections at 64, bounds handshake/header waits,
and ends a connection after its fifteen-minute admission lifetime. Each
protected request rechecks certificate validity and current durable admission.
Database expiry/revocation remains authoritative even while TLS stays open.

## Revoke an enrolled key

```bash
tachi a2a remote revoke --config /absolute/gateway.json \
  --admission admission-verified-EXACT-ID
```

The operator selects a real mTLS admission emitted by this issuer/domain.
The command appends to the existing revocation ledger. For `mtls-peer/v1`,
revoking one admission retires its SPKI key under that issuer: other current
connections using the key lose authority and fresh connections cannot restore
it. Existing receipts remain readable as history. Re-enrollment requires a new
key and an explicit enrollment update/restart; removing a config entry alone
is not the durable revocation operation. A never-used key can be removed from
configuration before any admission exists. `device-envelope/v1` retains its
original per-admission revocation semantics.

## Required host handoff before execution

Filesystem checks protect against other OS users, **not a worker with the
gateway's UID**. Run the gateway authority under a separately protected account
or equivalently demonstrated host boundary. The current native write sandbox,
private HOME and loopback address do not establish key confidentiality or prevent
use of an authority API. No self-reported isolation flag enables execution.

For owner-Mac/DGX2 activation, record the candidate SHA, actual OS and account
separation, endpoint/operator and pass-through relay bindings, certificate/key
rotation policy, and worker attempts to read keys or use the gateway's authority.
Do not send credentials or private material to public issues. Real Cloud→Mac
and Mac→DGX2 job, refusal, cancellation and cleanup acceptance remain #1934 work.

Portable/local verification uses the existing dogfood runner:

```bash
python3 scripts/dogfood_e2e.py --tachi /absolute/candidate/tachi --only remote-admission
```

The scenario uses temporary certificates and stores, exercises actual TLS and
the packaged CLI, and requires OpenSSL. Its evidence is transport/admission
behavior on that host, not owner-Mac deployment or agent-memory recall quality.

#!/usr/bin/env bash
# Back-compat shim: `tachi-hub` → `tachi hub …`
set -euo pipefail
TACHI_BIN="$(dirname "$0")/tachi"
if [[ ! -x "$TACHI_BIN" ]]; then
  TACHI_BIN="$(command -v tachi || true)"
fi
if [[ -z "$TACHI_BIN" || ! -x "$TACHI_BIN" ]]; then
  echo "tachi-hub: tachi binary not found" >&2
  exit 1
fi
exec "$TACHI_BIN" hub "$@"

#!/bin/bash
# Build/install the compatibility server + cleaner; see docs/engineering/operations/legacy-entrypoints.md.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"
exec python3 "$SCRIPT_DIR/scripts/install_legacy_binaries.py"

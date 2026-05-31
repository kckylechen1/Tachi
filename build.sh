#!/bin/bash
# Build release memory-server and install to ./bin/memory-server (symlinked as ~/bin/tachi).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

INSTALL_BIN="${INSTALL_BIN:-$SCRIPT_DIR/bin/memory-server}"
TACHI_LINK="${TACHI_LINK:-${HOME}/bin/tachi}"
SKIP_LINK="${SKIP_LINK:-0}"

echo ">> cargo build --release -p memory-server --locked"
cargo build --release -p memory-server --locked

SRC="$SCRIPT_DIR/target/release/memory-server"
if [[ ! -f "$SRC" ]]; then
  echo "error: release binary missing at $SRC" >&2
  exit 1
fi

mkdir -p "$(dirname "$INSTALL_BIN")"
cp -f "$SRC" "$INSTALL_BIN"
chmod +x "$INSTALL_BIN"
xattr -c "$INSTALL_BIN" 2>/dev/null || true
if [[ "$(uname -s)" == "Darwin" ]]; then
  codesign -s - -f "$INSTALL_BIN" >/dev/null 2>&1 || true
fi

if [[ "$SKIP_LINK" != "1" ]]; then
  mkdir -p "$(dirname "$TACHI_LINK")"
  ln -sfn "$INSTALL_BIN" "$TACHI_LINK"
  echo ">> linked $TACHI_LINK -> $INSTALL_BIN"
fi

echo ">> installed $("$INSTALL_BIN" --version 2>&1)"
ls -lh "$INSTALL_BIN"
if [[ -L "$TACHI_LINK" ]]; then
  ls -lh "$TACHI_LINK"
fi

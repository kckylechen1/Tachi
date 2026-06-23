#!/bin/bash
# Build release binaries and install them to stable user-level paths.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

INSTALL_BIN="${INSTALL_BIN:-${HOME}/.cargo/bin/memory-server}"
INSTALL_CLEAN_BIN="${INSTALL_CLEAN_BIN:-${HOME}/.cargo/bin/tachi-clean}"
TACHI_LINK="${TACHI_LINK:-${HOME}/bin/tachi}"
CLEAN_LINK="${CLEAN_LINK:-${HOME}/bin/tachi-clean}"
SKIP_LINK="${SKIP_LINK:-0}"

echo ">> cargo build --release -p memory-server -p tachi-clean --locked"
cargo build --release -p memory-server -p tachi-clean --locked

SRC="$SCRIPT_DIR/target/release/memory-server"
if [[ ! -f "$SRC" ]]; then
  echo "error: release binary missing at $SRC" >&2
  exit 1
fi
CLEAN_SRC="$SCRIPT_DIR/target/release/tachi-clean"
if [[ ! -f "$CLEAN_SRC" ]]; then
  echo "error: cleaner binary missing at $CLEAN_SRC" >&2
  exit 1
fi

mkdir -p "$(dirname "$INSTALL_BIN")"
cp -f "$SRC" "$INSTALL_BIN"
chmod +x "$INSTALL_BIN"
xattr -c "$INSTALL_BIN" 2>/dev/null || true
if [[ "$(uname -s)" == "Darwin" ]]; then
  codesign -s - -f "$INSTALL_BIN" >/dev/null 2>&1 || true
fi

mkdir -p "$(dirname "$INSTALL_CLEAN_BIN")"
cp -f "$CLEAN_SRC" "$INSTALL_CLEAN_BIN"
chmod +x "$INSTALL_CLEAN_BIN"
xattr -c "$INSTALL_CLEAN_BIN" 2>/dev/null || true
if [[ "$(uname -s)" == "Darwin" ]]; then
  codesign -s - -f "$INSTALL_CLEAN_BIN" >/dev/null 2>&1 || true
fi

if [[ "$SKIP_LINK" != "1" ]]; then
  mkdir -p "$(dirname "$TACHI_LINK")"
  ln -sfn "$INSTALL_BIN" "$TACHI_LINK"
  echo ">> linked $TACHI_LINK -> $INSTALL_BIN"
  mkdir -p "$(dirname "$CLEAN_LINK")"
  ln -sfn "$INSTALL_CLEAN_BIN" "$CLEAN_LINK"
  echo ">> linked $CLEAN_LINK -> $INSTALL_CLEAN_BIN"
fi

echo ">> installed $("$INSTALL_BIN" --version 2>&1)"
ls -lh "$INSTALL_BIN"
echo ">> installed $("$INSTALL_CLEAN_BIN" --help 2>&1 | head -1)"
ls -lh "$INSTALL_CLEAN_BIN"
if [[ -L "$TACHI_LINK" ]]; then
  ls -lh "$TACHI_LINK"
fi
if [[ -L "$CLEAN_LINK" ]]; then
  ls -lh "$CLEAN_LINK"
fi
